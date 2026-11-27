// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
use super::*;

fn early_cli_success(arguments: &[OsString]) -> Option<glib::ExitCode> {
    (arguments.get(1).is_some_and(|value| value == "--version")).then_some(glib::ExitCode::from(17))
}

fn bind_ok(init: &GettextInit) -> Result<(), String> {
    if init.domain().is_empty() {
        Err("domain missing".to_owned())
    } else {
        Ok(())
    }
}

/// The hosted path must bind the catalog itself, never through the app's
/// standalone binder.
///
/// That binder ends in `textdomain`, which is process-global: running it
/// from a module would make the last product loaded answer for every other
/// product sharing the process.
#[test]
fn hosted_binding_skips_standalone_binder() {
    fn bind_must_not_run(_init: &GettextInit) -> Result<(), String> {
        panic!("the hosted path must not run the standalone gettext binder");
    }

    let init = GettextInit::new("bigdemo", bind_must_not_run).with_locale_dir("/usr/share/locale");
    init.bind_catalog()
        .expect("binding a catalog needs no application binder");
}

#[test]
fn hosted_binding_reports_catalogless_domain() {
    let init = GettextInit::new("bigdemo", bind_ok);
    assert!(init.bind_catalog().is_err());
}

#[test]
fn spec_requires_application_id() {
    assert_eq!(
        AppShellSpec::new(" ").validate(),
        Err(AppShellSpecError::EmptyApplicationId)
    );
}

#[test]
fn spec_requires_valid_gapplication_id() {
    assert_eq!(
        AppShellSpec::new("not valid").validate(),
        Err(AppShellSpecError::InvalidApplicationId)
    );
}

#[test]
fn spec_rejects_empty_css() {
    assert_eq!(
        AppShellSpec::new("br.com.biglinux.Demo")
            .base_css(" ")
            .validate(),
        Err(AppShellSpecError::EmptyCss)
    );
}

#[test]
fn spec_rejects_empty_gettext_domain() {
    let gettext = GettextInit::new(" ", bind_ok);
    assert_eq!(
        AppShellSpec::new("br.com.biglinux.Demo")
            .gettext(gettext)
            .validate(),
        Err(AppShellSpecError::EmptyGettextDomain)
    );
}

#[test]
fn spec_rejects_empty_gettext_locale_dir() {
    let gettext = GettextInit::new("bigdemo", bind_ok).with_locale_dir(" ");
    assert_eq!(
        AppShellSpec::new("br.com.biglinux.Demo")
            .gettext(gettext)
            .validate(),
        Err(AppShellSpecError::EmptyGettextLocaleDir)
    );
}

#[test]
fn activation_error_always_returns_nonzero_status() {
    let error = AppShellActivationError::new("missing argument", 0);
    assert_eq!(error.message(), "missing argument");
    assert_eq!(error.status(), 1);
}

#[test]
fn plain_glib_object_is_no_root() {
    let root = glib::Object::new::<glib::Object>();
    assert!(component_root_window(&root).is_none());
}

#[test]
fn present_rejects_non_window_root() {
    let error = select_presentation_window::<&str>(
        AppShellActivationOutcome::PresentCurrent,
        false,
        None,
        || Some("secondary"),
    )
    .expect_err("PresentCurrent requires a GTK window root");

    assert_eq!(error.message(), NON_WINDOW_PRESENTATION_ERROR);
    assert_eq!(error.status(), 1);
}

#[test]
fn handled_accepts_a_non_window_root() {
    assert_eq!(
        select_presentation_window(AppShellActivationOutcome::Handled, false, None, || Some(
            "secondary"
        ),),
        Ok(None)
    );
}

#[test]
fn destroyed_primary_selects_a_secondary_window() {
    assert_eq!(
        select_presentation_window(
            AppShellActivationOutcome::PresentCurrent,
            true,
            None,
            || Some("secondary"),
        ),
        Ok(Some("secondary"))
    );
}

#[test]
fn destroyed_primary_fails_without_windows() {
    let error = select_presentation_window::<&str>(
        AppShellActivationOutcome::PresentCurrent,
        true,
        None,
        || None,
    )
    .expect_err("presentation requires a remaining application window");

    assert_eq!(error.message(), NO_WINDOW_PRESENTATION_ERROR);
}

/// A root that fails the test if `AppShell` gets as far as building it.
struct NeverBuilt;

impl relm4::SimpleComponent for NeverBuilt {
    type Init = ();
    type Input = ();
    type Output = ();
    type Root = gtk::Window;
    type Widgets = ();

    fn init_root() -> Self::Root {
        panic!("the root must not be built");
    }

    fn init(
        (): Self::Init,
        _root: Self::Root,
        _sender: relm4::ComponentSender<Self>,
    ) -> relm4::ComponentParts<Self> {
        panic!("the root must not be built");
    }
}

impl AppShellComponent for NeverBuilt {
    fn init_for_activation(
        _context: &AppShellContext,
        _request: AppShellActivationRequest,
    ) -> Result<Self::Init, AppShellActivationError> {
        panic!("the root must not be activated");
    }

    fn input_for_activation(
        _request: AppShellActivationRequest,
    ) -> Result<Self::Input, AppShellActivationError> {
        panic!("the root must not be activated");
    }
}

#[test]
fn early_cli_runs_before_validation() {
    let exit = run_root_with_args::<NeverBuilt, &str>(
        AppShellSpec::new("invalid id")
            .early_cli(early_cli_success)
            .start_threads(|| panic!("an early answer starts no thread")),
        vec!["demo", "--version"],
    );

    assert_eq!(exit, glib::ExitCode::from(17));
}

/// The test harness runs each test on a thread of its own, so `run` sees a
/// second thread and refuses before the early answer, which would give 17.
#[test]
fn run_refused_beside_thread() {
    let exit = AppShell::run::<NeverBuilt>(
        AppShellSpec::new("br.com.biglinux.Demo").early_cli(|_| Some(glib::ExitCode::from(17))),
    );

    assert_eq!(exit, glib::ExitCode::FAILURE);
}

#[cfg(unix)]
#[test]
fn early_cli_preserves_non_utf8_arguments() {
    use std::os::unix::ffi::OsStringExt;

    fn detect_non_utf8(arguments: &[OsString]) -> Option<glib::ExitCode> {
        (arguments.get(1).and_then(|value| value.to_str()).is_none())
            .then_some(glib::ExitCode::SUCCESS)
    }

    let arguments = vec![
        OsString::from("demo"),
        OsString::from_vec(vec![b'f', b'o', 0x80]),
    ];
    let exit = run_early_cli(
        &AppShellSpec::new("br.com.biglinux.Demo").early_cli(detect_non_utf8),
        &arguments,
    );

    assert_eq!(exit, Some(glib::ExitCode::SUCCESS));
}

#[cfg(unix)]
#[test]
fn gapplication_boundary_preserves_non_utf8_arguments() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let received = Rc::new(RefCell::new(Vec::<OsString>::new()));
    let received_for_command_line = Rc::clone(&received);
    let app = gio::Application::builder()
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE | gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_command_line(move |_, command_line| {
        *received_for_command_line.borrow_mut() = command_line.arguments();
        glib::ExitCode::SUCCESS
    });
    let arguments = vec![
        OsString::from("appkit-native-argv"),
        OsString::from_vec(b"file-\xff.txt".to_vec()),
    ];

    assert_eq!(
        run_application_with_os_args(&app, &arguments),
        glib::ExitCode::SUCCESS
    );
    assert_eq!(
        received.borrow()[1].as_os_str().as_bytes(),
        b"file-\xff.txt"
    );
}

#[test]
fn gettext_without_locale_dir_is_identity() {
    let init = GettextInit::standard(
        "big-app-kit-missing-locale-test",
        "/tmp/big-app-kit-locale-dir-that-should-not-exist",
    );

    init.bind()
        .expect("missing catalogs should not fail binding");

    assert_eq!(
        gettextrs::gettext("Missing catalog keeps the msgid"),
        "Missing catalog keeps the msgid"
    );
}

#[test]
#[ignore = "display: needs a GTK display"]
fn boot_exits_after_last_window() {
    use std::sync::atomic::{AtomicBool, Ordering};

    static BUILT: AtomicBool = AtomicBool::new(false);
    static THREADS_STARTED: AtomicBool = AtomicBool::new(false);

    struct SmokeRoot;

    impl relm4::SimpleComponent for SmokeRoot {
        type Init = ();
        type Input = ();
        type Output = ();
        type Root = adw::ApplicationWindow;
        type Widgets = ();

        fn init_root() -> Self::Root {
            adw::ApplicationWindow::builder()
                .title("AppShell smoke")
                .build()
        }

        fn init(
            (): Self::Init,
            root: Self::Root,
            _sender: relm4::ComponentSender<Self>,
        ) -> relm4::ComponentParts<Self> {
            assert!(
                THREADS_STARTED.load(Ordering::SeqCst),
                "the program's threads start before its first window"
            );
            BUILT.store(true, Ordering::SeqCst);
            let window_for_close = root;
            glib::idle_add_local_once(move || window_for_close.close());
            relm4::ComponentParts {
                model: Self,
                widgets: (),
            }
        }
    }

    impl AppShellComponent for SmokeRoot {
        fn init_for_activation(
            _context: &AppShellContext,
            request: AppShellActivationRequest,
        ) -> Result<Self::Init, AppShellActivationError> {
            request.succeed();
            Ok(())
        }

        fn input_for_activation(
            request: AppShellActivationRequest,
        ) -> Result<Self::Input, AppShellActivationError> {
            request.succeed();
            Ok(())
        }
    }

    let exit = run_root_with_args::<SmokeRoot, &str>(
        AppShellSpec::new("br.com.biglinux.AppKitSmoke")
            .start_threads(|| THREADS_STARTED.store(true, Ordering::SeqCst)),
        vec!["appkit-smoke"],
    );
    assert_eq!(exit, glib::ExitCode::SUCCESS);
    assert!(BUILT.load(Ordering::SeqCst));
}

#[test]
#[ignore = "display: needs a GTK display"]
fn deferred_failure_returns_after_main_loop() {
    struct DeferredFailureRoot;

    impl relm4::SimpleComponent for DeferredFailureRoot {
        type Init = AppShellActivationRequest;
        type Input = ();
        type Output = ();
        type Root = glib::Object;
        type Widgets = ();

        fn init_root() -> Self::Root {
            glib::Object::new::<glib::Object>()
        }

        fn init(
            request: Self::Init,
            _root: Self::Root,
            _sender: relm4::ComponentSender<Self>,
        ) -> relm4::ComponentParts<Self> {
            glib::idle_add_local_once(move || {
                request.fail(AppShellActivationError::new("deferred local failure", 23));
            });
            relm4::ComponentParts {
                model: Self,
                widgets: (),
            }
        }
    }

    impl AppShellComponent for DeferredFailureRoot {
        fn init_for_activation(
            _context: &AppShellContext,
            request: AppShellActivationRequest,
        ) -> Result<Self::Init, AppShellActivationError> {
            Ok(request)
        }

        fn input_for_activation(
            request: AppShellActivationRequest,
        ) -> Result<Self::Input, AppShellActivationError> {
            request.fail(AppShellActivationError::new(
                "unexpected repeated activation",
                24,
            ));
            Ok(())
        }

        fn activation_outcome(_activation: &AppShellActivation) -> AppShellActivationOutcome {
            AppShellActivationOutcome::Handled
        }
    }

    let exit = run_root_with_args::<DeferredFailureRoot, &str>(
        AppShellSpec::new("br.com.biglinux.AppKitDeferredLocalStatus"),
        vec!["appkit-deferred-local-status"],
    );

    assert_eq!(exit, glib::ExitCode::from(23));
}

/// A local launch that fails at once keeps its status while the window it
/// opened keeps GApplication's main loop turning.
#[test]
#[ignore = "display: needs a GTK display"]
fn immediate_failure_survives_main_loop() {
    struct FailingWindowRoot;

    impl relm4::SimpleComponent for FailingWindowRoot {
        type Init = ();
        type Input = ();
        type Output = ();
        type Root = adw::ApplicationWindow;
        type Widgets = ();

        fn init_root() -> Self::Root {
            adw::ApplicationWindow::builder().build()
        }

        fn init(
            (): Self::Init,
            root: Self::Root,
            _sender: relm4::ComponentSender<Self>,
        ) -> relm4::ComponentParts<Self> {
            glib::idle_add_local_once(move || root.close());
            relm4::ComponentParts {
                model: Self,
                widgets: (),
            }
        }
    }

    impl AppShellComponent for FailingWindowRoot {
        fn init_for_activation(
            _context: &AppShellContext,
            request: AppShellActivationRequest,
        ) -> Result<Self::Init, AppShellActivationError> {
            request.fail(AppShellActivationError::new("immediate local failure", 23));
            Ok(())
        }

        fn input_for_activation(
            request: AppShellActivationRequest,
        ) -> Result<Self::Input, AppShellActivationError> {
            request.fail(AppShellActivationError::new(
                "unexpected repeated activation",
                24,
            ));
            Ok(())
        }
    }

    let exit = run_root_with_args::<FailingWindowRoot, &str>(
        AppShellSpec::new("br.com.biglinux.AppKitImmediateLocalStatus"),
        vec!["appkit-immediate-local-status"],
    );

    assert_eq!(exit, glib::ExitCode::from(23));
}

/// Only fill the gap, and only where the gap breaks typing.
///
/// Getting this wrong in either direction is user-visible: forcing
/// `simple` over a running fcitx5 takes away that person's input method,
/// and skipping it on a bare Wayland session loses every accented letter.
#[test]
fn simple_input_method_fills_gap() {
    let wayland = Some(OsStr::new("wayland"));
    let x11 = Some(OsStr::new("x11"));
    let kwin = Some(OsStr::new("big:KDE"));

    assert!(needs_simple_input_method(wayland, kwin, None, || false));
    assert!(needs_simple_input_method(
        Some(OsStr::new("Wayland")),
        None,
        Some(OsStr::new("")),
        || false
    ));

    assert!(
        !needs_simple_input_method(wayland, kwin, Some(OsStr::new("ibus")), || false),
        "a chosen module stands"
    );
    assert!(
        !needs_simple_input_method(wayland, kwin, None, || true),
        "a running daemon configures the variable itself"
    );
    assert!(
        !needs_simple_input_method(
            wayland,
            Some(OsStr::new("big-compositor:big")),
            None,
            || false
        ),
        "big-compositor composes, and text-input raises the on-screen keyboard"
    );
    assert!(
        !needs_simple_input_method(x11, None, None, || false),
        "X11 composes without this"
    );
    assert!(
        !needs_simple_input_method(None, None, None, || false),
        "an unknown session type is not ours to change"
    );
}

/// An input method daemon counts only when it runs as this user.
#[test]
fn other_users_daemon_ignored() {
    let proc =
        std::env::temp_dir().join(format!("big-app-kit-proc-{}", glib::uuid_string_random()));
    for (process, comm) in [("1234", "fcitx5\n"), ("self", "ibus-daemon\n")] {
        std::fs::create_dir_all(proc.join(process)).expect("process directory");
        std::fs::write(proc.join(process).join("comm"), comm).expect("comm");
    }
    let own = big_os_kit::microdaemon::current_effective_user_id();
    let for_own = input_method_daemon_runs(&proc, own);
    let for_other = input_method_daemon_runs(&proc, own.wrapping_add(1));
    std::fs::remove_dir_all(&proc).expect("scratch removed");
    assert!(for_own);
    assert!(!for_other);
}

/// A launch that hands its arguments to a running instance never reaches
/// `startup`: it must not have paid for the toolkit by then.
#[test]
fn boot_leaves_toolkit_alone() {
    assert!(!gtk::is_initialized(), "the toolkit starts only on demand");
    boot_application(&AppShellSpec::new("br.com.biglinux.BootLeavesToolkit"))
        .expect("a valid spec boots");
    assert!(!gtk::is_initialized());
}
