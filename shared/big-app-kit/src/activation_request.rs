// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
use std::cell::RefCell;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gio::prelude::{
    ApplicationCommandLineExt, ApplicationCommandLineExtManual, ApplicationExtManual, FileExt,
};

const DROPPED_ACTIVATION_ERROR: &str = "activation request dropped without a result";
const UNDELIVERED_ACTIVATION_ERROR: &str =
    "activation request was not delivered to the root component";

/// One local or remote `GApplication` command-line activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppShellActivation {
    arguments: Vec<OsString>,
    cwd: Option<PathBuf>,
}

impl AppShellActivation {
    pub(crate) fn from_command_line(command_line: &gio::ApplicationCommandLine) -> Self {
        Self {
            arguments: command_line.arguments(),
            cwd: command_line.cwd(),
        }
    }

    /// Full argument vector, including the invoked program name.
    #[must_use]
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    /// Working directory of the invoking process, including remote activations.
    #[must_use]
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Arguments after the executable name, interpreted as file or URI open
    /// targets.
    ///
    /// Relative paths are resolved against the invoking process's working
    /// directory, including remote activations. URI arguments remain URIs.
    /// Applications with switches must parse [`Self::arguments`] instead of
    /// treating every trailing argument as an open target. This convenience
    /// projection replaces non-UTF-8 path bytes lossily; use
    /// [`Self::arguments`] when the native OS form is required.
    #[must_use]
    pub fn file_arguments(&self) -> Vec<String> {
        self.arguments
            .iter()
            .skip(1)
            .map(|argument| {
                let file = self.cwd.as_ref().map_or_else(
                    || gio::File::for_commandline_arg(argument),
                    |cwd| gio::File::for_commandline_arg_and_cwd(argument, cwd),
                );
                file.path().map_or_else(
                    || file.uri().to_string(),
                    |path| path.to_string_lossy().into_owned(),
                )
            })
            .collect()
    }
}

/// User-facing failure returned to a remote or immediately completed local
/// activation process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppShellActivationError {
    message: String,
    status: u8,
}

impl AppShellActivationError {
    /// Build an activation failure. Status zero is normalized to one.
    #[must_use]
    pub fn new(message: impl Into<String>, status: u8) -> Self {
        Self {
            message: message.into(),
            status: status.max(1),
        }
    }

    /// Concrete diagnostic printed to the invoking process.
    #[must_use]
    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    /// Non-zero process status returned to the invoking process.
    #[must_use]
    pub(crate) const fn status(&self) -> u8 {
        self.status
    }
}

/// Linear activation request delivered through a root component's typed
/// `Init` or `Input` value.
///
/// Call [`succeed`](Self::succeed) only after the requested activation has been
/// accepted, or [`fail`](Self::fail) with a concrete diagnostic. A component
/// may retain the request while it performs asynchronous work. When processing
/// is truly deferred, AppShell retains the invoking command line and one
/// application hold until the request completes. Dropping an unfinished
/// request records a failure instead of silently completing successfully.
/// Remote clients receive its non-zero status. AppShell also retains the
/// completed status of a deferred local activation and returns it if that
/// primary process exits.
/// The request is main-context-only; workers return typed results to the owning
/// component instead of moving this value off the GTK thread.
#[must_use = "an activation request must be completed or retained by the root component"]
pub struct AppShellActivationRequest {
    activation: AppShellActivation,
    completion: Option<ActivationRequestCompletion>,
}

impl AppShellActivationRequest {
    pub(crate) fn new(activation: AppShellActivation) -> (Self, ActivationRequestTracker) {
        let tracker = ActivationRequestTracker::new();
        (
            Self {
                activation,
                completion: Some(ActivationRequestCompletion {
                    tracker: tracker.clone(),
                }),
            },
            tracker,
        )
    }

    /// Activation arguments and invoking-process context.
    #[must_use]
    pub const fn activation(&self) -> &AppShellActivation {
        &self.activation
    }

    /// Complete the request successfully.
    pub fn succeed(mut self) {
        self.complete(Ok(()));
    }

    /// Complete the request with a diagnostic and non-zero remote process
    /// status.
    pub fn fail(mut self, error: AppShellActivationError) {
        self.complete(Err(error));
    }

    fn complete(&mut self, result: ActivationResult) {
        if let Some(completion) = self.completion.take() {
            completion.tracker.complete(result);
        }
    }
}

impl fmt::Debug for AppShellActivationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppShellActivationRequest")
            .field("activation", &self.activation)
            .field("is_completed", &self.completion.is_none())
            .finish()
    }
}

impl Drop for AppShellActivationRequest {
    fn drop(&mut self) {
        if self.completion.is_some() {
            self.complete(Err(dropped_activation_error()));
        }
    }
}

type ActivationResult = Result<(), AppShellActivationError>;

struct ActivationRequestCompletion {
    tracker: ActivationRequestTracker,
}

#[derive(Clone)]
pub(crate) struct ActivationRequestTracker {
    inner: Rc<ActivationRequestInner>,
}

impl ActivationRequestTracker {
    fn new() -> Self {
        Self {
            inner: Rc::new(ActivationRequestInner {
                state: RefCell::new(ActivationRequestState::Preparing),
            }),
        }
    }

    pub(crate) fn arm(&self) {
        let mut state = self.inner.state.borrow_mut();
        if matches!(*state, ActivationRequestState::Preparing) {
            *state = ActivationRequestState::Pending;
        }
    }

    pub(crate) fn fail_before_defer(&self, error: AppShellActivationError) {
        let target = {
            let mut state = self.inner.state.borrow_mut();
            match std::mem::replace(&mut *state, ActivationRequestState::Finished) {
                ActivationRequestState::Deferred(target) => Some(target),
                ActivationRequestState::Finished => None,
                _ => {
                    *state = ActivationRequestState::Immediate(Err(error));
                    return;
                }
            }
        };
        if let Some(target) = target {
            target.finish(Err(error));
        }
    }

    pub(crate) fn resolve_or_defer(
        &self,
        app: &gio::Application,
        command_line: &gio::ApplicationCommandLine,
    ) -> Option<ActivationResult> {
        match self.begin_defer() {
            BeginDefer::Immediate(result) => Some(result),
            BeginDefer::Install => {
                let target = Box::new(GioCommandLineTarget {
                    command_line: command_line.clone(),
                    application_hold: app.hold(),
                });
                self.install_deferred(target)
            }
        }
    }

    fn begin_defer(&self) -> BeginDefer {
        let mut state = self.inner.state.borrow_mut();
        match std::mem::replace(&mut *state, ActivationRequestState::Finished) {
            ActivationRequestState::Immediate(result) => BeginDefer::Immediate(result),
            ActivationRequestState::Pending => {
                *state = ActivationRequestState::Deferring;
                BeginDefer::Install
            }
            ActivationRequestState::Preparing => BeginDefer::Immediate(Err(
                AppShellActivationError::new(UNDELIVERED_ACTIVATION_ERROR, 1),
            )),
            ActivationRequestState::Deferring
            | ActivationRequestState::Deferred(_)
            | ActivationRequestState::Finished => BeginDefer::Immediate(Err(
                AppShellActivationError::new("activation request completion state is invalid", 1),
            )),
        }
    }

    fn install_deferred(
        &self,
        target: Box<dyn DeferredActivationTarget>,
    ) -> Option<ActivationResult> {
        let mut state = self.inner.state.borrow_mut();
        match std::mem::replace(&mut *state, ActivationRequestState::Finished) {
            ActivationRequestState::Deferring => {
                *state = ActivationRequestState::Deferred(target);
                None
            }
            ActivationRequestState::Immediate(result) => Some(result),
            _ => Some(Err(AppShellActivationError::new(
                "activation request could not enter deferred processing",
                1,
            ))),
        }
    }

    fn complete(&self, result: ActivationResult) {
        let target = {
            let mut state = self.inner.state.borrow_mut();
            match std::mem::replace(&mut *state, ActivationRequestState::Finished) {
                ActivationRequestState::Preparing
                | ActivationRequestState::Pending
                | ActivationRequestState::Deferring => {
                    *state = ActivationRequestState::Immediate(result);
                    return;
                }
                ActivationRequestState::Deferred(target) => Some(target),
                ActivationRequestState::Immediate(existing) => {
                    *state = ActivationRequestState::Immediate(existing);
                    None
                }
                ActivationRequestState::Finished => None,
            }
        };
        if let Some(target) = target {
            target.finish(result);
        }
    }
}

struct ActivationRequestInner {
    state: RefCell<ActivationRequestState>,
}

impl Drop for ActivationRequestInner {
    fn drop(&mut self) {
        if let ActivationRequestState::Deferred(target) =
            std::mem::replace(self.state.get_mut(), ActivationRequestState::Finished)
        {
            target.finish(Err(dropped_activation_error()));
        }
    }
}

enum ActivationRequestState {
    Preparing,
    Pending,
    Immediate(ActivationResult),
    Deferring,
    Deferred(Box<dyn DeferredActivationTarget>),
    Finished,
}

enum BeginDefer {
    Immediate(ActivationResult),
    Install,
}

trait DeferredActivationTarget {
    fn finish(self: Box<Self>, result: ActivationResult);
}

struct GioCommandLineTarget {
    command_line: gio::ApplicationCommandLine,
    application_hold: gio::ApplicationHoldGuard,
}

impl DeferredActivationTarget for GioCommandLineTarget {
    fn finish(self: Box<Self>, result: ActivationResult) {
        let Self {
            command_line,
            application_hold,
        } = *self;
        let exit_code = match &result {
            Ok(()) => glib::ExitCode::SUCCESS,
            Err(error) => glib::ExitCode::from(error.status()),
        };
        command_line.set_exit_code(exit_code);
        if let Err(error) = result {
            print_command_line_error(&command_line, error.message());
        }
        command_line.done();
        drop(application_hold);
    }
}

pub(crate) fn print_command_line_error(command_line: &gio::ApplicationCommandLine, message: &str) {
    command_line.printerr_literal(&format!("{}\n", message.replace('\0', "�")));
}

fn dropped_activation_error() -> AppShellActivationError {
    AppShellActivationError::new(DROPPED_ACTIVATION_ERROR, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordedResults(Rc<RefCell<Vec<ActivationResult>>>);

    impl RecordedResults {
        fn target(&self) -> Box<dyn DeferredActivationTarget> {
            Box::new(RecordingTarget(Rc::clone(&self.0)))
        }

        fn take(&self) -> Vec<ActivationResult> {
            std::mem::take(&mut *self.0.borrow_mut())
        }
    }

    struct RecordingTarget(Rc<RefCell<Vec<ActivationResult>>>);

    impl DeferredActivationTarget for RecordingTarget {
        fn finish(self: Box<Self>, result: ActivationResult) {
            self.0.borrow_mut().push(result);
        }
    }

    fn request() -> (AppShellActivationRequest, ActivationRequestTracker) {
        AppShellActivationRequest::new(AppShellActivation {
            arguments: vec![OsString::from("demo")],
            cwd: None,
        })
    }

    #[test]
    fn deferred_success_finishes_once() {
        let (request, tracker) = request();
        let recorded = RecordedResults::default();
        tracker.arm();
        assert!(matches!(tracker.begin_defer(), BeginDefer::Install));
        assert!(tracker.install_deferred(recorded.target()).is_none());

        request.succeed();

        assert_eq!(recorded.take(), [Ok(())]);
    }

    #[test]
    fn immediate_success_needs_no_target() {
        let (request, tracker) = request();
        tracker.arm();

        request.succeed();

        assert!(matches!(
            tracker.begin_defer(),
            BeginDefer::Immediate(Ok(()))
        ));
    }

    #[test]
    fn immediate_failure_preserves_status_and_diagnostic() {
        let (request, tracker) = request();
        tracker.arm();

        request.fail(AppShellActivationError::new("invalid activation", 23));

        let BeginDefer::Immediate(Err(error)) = tracker.begin_defer() else {
            panic!("explicit failure must resolve before deferral");
        };
        assert_eq!(error.message(), "invalid activation");
        assert_eq!(error.status(), 23);
    }

    #[test]
    fn driver_failure_overrides_premature_success() {
        let (request, tracker) = request();
        tracker.arm();
        request.succeed();

        tracker.fail_before_defer(AppShellActivationError::new("driver failed", 19));

        let BeginDefer::Immediate(Err(error)) = tracker.begin_defer() else {
            panic!("driver failure must override the unreported success");
        };
        assert_eq!(error.message(), "driver failed");
        assert_eq!(error.status(), 19);
    }

    #[test]
    fn driver_failure_overrides_drop() {
        let (request, tracker) = request();
        tracker.arm();
        drop(request);

        tracker.fail_before_defer(AppShellActivationError::new("presentation failed", 7));

        let BeginDefer::Immediate(Err(error)) = tracker.begin_defer() else {
            panic!("concrete driver failure must replace the automatic drop failure");
        };
        assert_eq!(error.message(), "presentation failed");
        assert_eq!(error.status(), 7);
    }

    #[test]
    fn completion_after_failure_is_inert() {
        let (request, tracker) = request();
        let recorded = RecordedResults::default();
        tracker.arm();
        assert!(matches!(tracker.begin_defer(), BeginDefer::Install));
        assert!(tracker.install_deferred(recorded.target()).is_none());

        tracker.fail_before_defer(AppShellActivationError::new("dispatch failed", 11));
        request.succeed();

        let results = recorded.take();
        let [Err(error)] = results.as_slice() else {
            panic!("forced failure must finish the deferred target exactly once");
        };
        assert_eq!(error.message(), "dispatch failed");
        assert_eq!(error.status(), 11);
    }

    #[test]
    fn deferred_failure_preserves_status_and_diagnostic() {
        let (request, tracker) = request();
        let recorded = RecordedResults::default();
        tracker.arm();
        assert!(matches!(tracker.begin_defer(), BeginDefer::Install));
        assert!(tracker.install_deferred(recorded.target()).is_none());

        request.fail(AppShellActivationError::new("module failed", 23));

        let results = recorded.take();
        let [Err(error)] = results.as_slice() else {
            panic!("deferred request must record one failure");
        };
        assert_eq!(error.message(), "module failed");
        assert_eq!(error.status(), 23);
    }

    #[test]
    fn dropping_deferred_request_returns_nonzero_failure() {
        let (request, tracker) = request();
        let recorded = RecordedResults::default();
        tracker.arm();
        assert!(matches!(tracker.begin_defer(), BeginDefer::Install));
        assert!(tracker.install_deferred(recorded.target()).is_none());

        drop(request);

        let results = recorded.take();
        let [Err(error)] = results.as_slice() else {
            panic!("dropped request must record one failure");
        };
        assert_eq!(error.message(), DROPPED_ACTIVATION_ERROR);
        assert_eq!(error.status(), 1);
    }

    #[test]
    fn request_dropped_before_dispatch_fails_immediately() {
        let (request, tracker) = request();
        drop(request);
        tracker.arm();

        let BeginDefer::Immediate(Err(error)) = tracker.begin_defer() else {
            panic!("dropped request must fail before deferral");
        };
        assert_eq!(error.message(), DROPPED_ACTIVATION_ERROR);
    }

    #[test]
    fn relative_arguments_resolve_against_invoker_cwd() {
        let activation = AppShellActivation {
            arguments: vec![OsString::from("demo"), OsString::from("relative")],
            cwd: Some(PathBuf::from("/invoker")),
        };

        assert_eq!(activation.arguments(), ["demo", "relative"]);
        assert_eq!(activation.cwd(), Some(Path::new("/invoker")));
        assert_eq!(activation.file_arguments(), ["/invoker/relative"]);
    }

    #[test]
    fn file_projection_keeps_paths_uris() {
        let activation = AppShellActivation {
            arguments: vec![
                OsString::from("bigdemo"),
                OsString::from("/tmp/a.txt"),
                OsString::from("smb://server/share/file.txt"),
            ],
            cwd: None,
        };

        assert_eq!(
            activation.file_arguments(),
            ["/tmp/a.txt", "smb://server/share/file.txt"]
        );
    }
}
