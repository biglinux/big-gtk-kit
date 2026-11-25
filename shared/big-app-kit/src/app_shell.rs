// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
use std::cell::RefCell;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::rc::Rc;

use glib::translate::{IntoGlib, ToGlibPtr};
use gtk::prelude::*;
use relm4::{Component, ComponentController, Controller};

use crate::activation_request::{
    AppShellActivation, AppShellActivationError, AppShellActivationRequest,
    print_command_line_error,
};
use crate::gettext_init::GettextInit;

/// Declarative AppShell boot contract.
///
/// The spec contains only process boot concerns. Window composition stays in
/// the caller-supplied Relm4 root component.
///
/// # Example
///
/// ```no_run
/// use big_app_kit::AppShellSpec;
///
/// let spec = AppShellSpec::new("br.com.biglinux.AppKitDemo")
///     .base_css(".demo-window { background: var(--window-bg-color); }")
///     .watch_dark_mode(true);
/// assert_eq!(spec.application_id(), "br.com.biglinux.AppKitDemo");
/// ```
#[derive(Debug, Clone)]
pub struct AppShellSpec {
    application_id: String,
    gettext: Option<GettextInit>,
    base_css: Option<&'static str>,
    watch_dark_mode: bool,
    resource_paths: &'static [&'static str],
    early_cli: Option<AppShellEarlyCli>,
}

/// Display-less command-line handler run before GTK and application startup.
///
/// Return an exit code when the arguments were handled, or `None` to continue
/// normal `GApplication` activation. Arguments preserve their native OS form.
pub type AppShellEarlyCli = fn(&[OsString]) -> Option<glib::ExitCode>;

impl AppShellSpec {
    /// Start a boot spec for a static or dynamically selected reverse-DNS
    /// application id.
    #[must_use]
    pub fn new(application_id: impl Into<String>) -> Self {
        Self {
            application_id: application_id.into(),
            gettext: None,
            base_css: None,
            watch_dark_mode: false,
            resource_paths: &[],
            early_cli: None,
        }
    }

    /// The application id used by `adw::Application` and its single-instance key.
    #[must_use]
    pub fn application_id(&self) -> &str {
        &self.application_id
    }

    /// Run `gettext` initialization before the first window.
    #[must_use]
    pub const fn gettext(mut self, gettext: GettextInit) -> Self {
        self.gettext = Some(gettext);
        self
    }

    /// Install app CSS on the default display before the first window.
    #[must_use]
    pub const fn base_css(mut self, css: &'static str) -> Self {
        self.base_css = Some(css);
        self
    }

    /// Connect the libadwaita dark-mode stream to typed input returned by
    /// [`AppShellComponent::input_for_dark_mode`].
    #[must_use]
    pub const fn watch_dark_mode(mut self, watch_dark_mode: bool) -> Self {
        self.watch_dark_mode = watch_dark_mode;
        self
    }

    /// Handle app-specific options such as `--help` or `--version` before GTK.
    #[must_use]
    pub const fn early_cli(mut self, handler: AppShellEarlyCli) -> Self {
        self.early_cli = Some(handler);
        self
    }

    /// Apply this spec's process-wide effects inside a process GTK already runs.
    ///
    /// The multicall-host path. [`AppShell::run`] owns the whole boot; a product
    /// loaded as a module is handed a live `GtkApplication` instead and would
    /// otherwise silently lose its catalog, its icon resources and its CSS.
    ///
    /// Two effects are left out. `adw::init` already ran, and the
    /// process-global color scheme belongs to whoever booted the process: a
    /// module that forced it would answer for every other product in the same
    /// process.
    ///
    /// Safe to call once per hosted surface: catalogs and icon resource paths
    /// are idempotent, and repeated CSS is applied only the first time.
    pub fn apply_to_running_process(&self) {
        if let Some(gettext) = &self.gettext
            && let Err(error) = gettext.bind_catalog()
        {
            log::warn!("app shell: hosted gettext bind failed: {error}");
        }
        register_resource_paths(self.resource_paths);
        if let Some(css) = self.base_css {
            big_relm4_components::css::install(css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
        install_focus_ring();
    }

    /// Validate the boot contract before it can drive GTK.
    ///
    /// # Errors
    ///
    /// Returns [`AppShellSpecError`] when the application id, CSS, gettext
    /// fields, or resource paths are empty.
    pub fn validate(&self) -> Result<(), AppShellSpecError> {
        require(
            !self.application_id.trim().is_empty(),
            AppShellSpecError::EmptyApplicationId,
        )?;
        require(
            gio::Application::id_is_valid(&self.application_id),
            AppShellSpecError::InvalidApplicationId,
        )?;
        if let Some(css) = self.base_css {
            require(!css.trim().is_empty(), AppShellSpecError::EmptyCss)?;
        }
        if let Some(gettext) = &self.gettext {
            gettext.validate()?;
        }
        for resource_path in self.resource_paths {
            require(
                !resource_path.trim().is_empty(),
                AppShellSpecError::EmptyResourcePath,
            )?;
        }
        Ok(())
    }
}

/// Error returned when an [`AppShellSpec`] cannot boot.
///
/// # Example
///
/// ```no_run
/// use big_app_kit::{AppShellSpec, AppShellSpecError};
///
/// let error = AppShellSpec::new("not valid").validate().unwrap_err();
/// assert_eq!(error, AppShellSpecError::InvalidApplicationId);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AppShellSpecError {
    /// Application id is empty or whitespace-only.
    EmptyApplicationId,
    /// Application id is not accepted by `GApplication`.
    InvalidApplicationId,
    /// Base CSS is empty or whitespace-only.
    EmptyCss,
    /// Gettext domain is empty or whitespace-only.
    EmptyGettextDomain,
    /// Gettext locale directory is empty or whitespace-only.
    EmptyGettextLocaleDir,
    /// Icon-theme resource path is empty or whitespace-only.
    EmptyResourcePath,
}

impl fmt::Display for AppShellSpecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyApplicationId => formatter.write_str("application id must not be empty"),
            Self::InvalidApplicationId => {
                formatter.write_str("application id must be a valid GApplication id")
            }
            Self::EmptyCss => formatter.write_str("base CSS must not be empty"),
            Self::EmptyGettextDomain => formatter.write_str("gettext domain must not be empty"),
            Self::EmptyGettextLocaleDir => {
                formatter.write_str("gettext locale directory must not be empty")
            }
            Self::EmptyResourcePath => formatter.write_str("resource path must not be empty"),
        }
    }
}

impl Error for AppShellSpecError {}

/// AppShell context passed to the graphical root component.
///
/// # Example
///
/// ```no_run
/// use big_app_kit::AppShellContext;
///
/// fn build_window(context: &AppShellContext) -> adw::ApplicationWindow {
///     adw::ApplicationWindow::builder()
///         .application(context.app())
///         .build()
/// }
/// ```
pub struct AppShellContext<'a> {
    app: &'a adw::Application,
}

impl<'a> AppShellContext<'a> {
    fn new(app: &'a adw::Application) -> Self {
        Self { app }
    }

    /// The running libadwaita application.
    #[must_use]
    pub const fn app(&self) -> &'a adw::Application {
        self.app
    }
}

/// Whether AppShell should present the current primary window after handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppShellActivationOutcome {
    /// Present the current primary window after the activation succeeds.
    PresentCurrent,
    /// The app handled presentation itself, for example by opening a new window.
    Handled,
}

/// Relm4 root component driven by AppShell activations.
///
/// The first activation constructs one controller from typed `Init`; later
/// activations reuse it through typed `Input`. Normal roots are GTK windows; a
/// foreign-window coordinator may use another GLib object root only when every
/// activation returns [`AppShellActivationOutcome::Handled`].
pub trait AppShellComponent: Component {
    /// Synchronous validation failures return `Err` without first completing
    /// the request. A request moved into component state or input must later be
    /// consumed by exactly one `succeed` or `fail` call.
    ///
    /// Build the root component's initialization value for the first valid
    /// activation.
    ///
    /// # Errors
    ///
    /// [`AppShellActivationError`] for a request the application refuses.
    fn init_for_activation(
        context: &AppShellContext,
        request: AppShellActivationRequest,
    ) -> Result<Self::Init, AppShellActivationError>;

    /// Convert each valid activation after the first into a root input.
    ///
    /// # Errors
    ///
    /// [`AppShellActivationError`] for a request the application refuses.
    fn input_for_activation(
        request: AppShellActivationRequest,
    ) -> Result<Self::Input, AppShellActivationError>;

    /// Choose whether AppShell presents the current application window after
    /// an activation.
    ///
    /// `PresentCurrent` requires a GTK window root. After its destruction, a
    /// later activation presents the active or another registered application
    /// window and fails when none remains.
    fn activation_outcome(_activation: &AppShellActivation) -> AppShellActivationOutcome {
        AppShellActivationOutcome::PresentCurrent
    }

    /// Convert a libadwaita dark-mode transition into typed root input.
    ///
    /// Returning `None` means this component does not consume the stream.
    /// The method is called only when [`AppShellSpec::watch_dark_mode`] is
    /// enabled; AppShell never mutates application state through a static
    /// callback.
    fn input_for_dark_mode(_is_dark: bool) -> Option<Self::Input> {
        None
    }
}

const NON_WINDOW_PRESENTATION_ERROR: &str = "PresentCurrent requires a GTK window component root";
const NO_WINDOW_PRESENTATION_ERROR: &str = "AppShell component requested PresentCurrent after its primary window was destroyed, but no application window remains";
const ACTIVATION_PANIC_ERROR: &str = "activation processing panicked";
const ACTIVATION_CHANNEL_ERROR: &str = "root component is no longer accepting activations";

struct AppShellController<C>
where
    C: Component,
    C::Root: IsA<glib::Object>,
{
    controller: Controller<C>,
    primary_window: Rc<RefCell<Option<gtk::Window>>>,
    has_primary_window: bool,
}

impl<C> AppShellController<C>
where
    C: Component,
    C::Root: IsA<glib::Object>,
{
    fn launch(init: C::Init) -> Self {
        let controller = C::builder().launch(init).detach();
        let primary_window = component_root_window(controller.widget());
        let has_primary_window = primary_window.is_some();
        let primary_window = Rc::new(RefCell::new(primary_window));
        if let Some(window) = primary_window.borrow().as_ref() {
            let primary_window_for_destroy = Rc::clone(&primary_window);
            window.connect_destroy(move |_| {
                primary_window_for_destroy.borrow_mut().take();
            });
        }
        Self {
            controller,
            primary_window,
            has_primary_window,
        }
    }

    fn emit(&self, input: C::Input) -> Result<(), AppShellActivationError> {
        self.controller
            .sender()
            .send(input)
            .map_err(|_| AppShellActivationError::new(ACTIVATION_CHANNEL_ERROR, 1))
    }

    fn primary_window(&self) -> Option<gtk::Window> {
        self.primary_window.borrow().clone()
    }

    fn bind_primary_window(&self, app: &adw::Application) {
        if let Some(window) = self.primary_window()
            && window.application().as_ref() != Some(app.upcast_ref())
        {
            window.set_application(Some(app));
        }
    }

    fn window_for_presentation(
        &self,
        app: &adw::Application,
        outcome: AppShellActivationOutcome,
        can_use_secondary_window: bool,
    ) -> Result<Option<gtk::Window>, AppShellActivationError> {
        select_presentation_window(
            outcome,
            self.has_primary_window,
            self.primary_window(),
            || {
                can_use_secondary_window
                    .then(|| fallback_application_window(app))
                    .flatten()
            },
        )
    }
}

fn component_root_window<R>(root: &R) -> Option<gtk::Window>
where
    R: IsA<glib::Object> + Clone,
{
    root.clone()
        .upcast::<glib::Object>()
        .downcast::<gtk::Window>()
        .ok()
}

fn select_presentation_window<T>(
    outcome: AppShellActivationOutcome,
    has_primary_window: bool,
    primary_window: Option<T>,
    fallback_window: impl FnOnce() -> Option<T>,
) -> Result<Option<T>, AppShellActivationError> {
    match outcome {
        AppShellActivationOutcome::Handled => Ok(None),
        AppShellActivationOutcome::PresentCurrent if !has_primary_window => Err(
            AppShellActivationError::new(NON_WINDOW_PRESENTATION_ERROR, 1),
        ),
        AppShellActivationOutcome::PresentCurrent => primary_window
            .or_else(fallback_window)
            .map(Some)
            .ok_or_else(|| AppShellActivationError::new(NO_WINDOW_PRESENTATION_ERROR, 1)),
    }
}

fn fallback_application_window(app: &adw::Application) -> Option<gtk::Window> {
    app.active_window()
        .or_else(|| app.windows().into_iter().next())
}

/// AppShell entry point for graphical Relm4 applications.
pub struct AppShell;

impl AppShell {
    /// Boot a Relm4 root component and retain its controller until the GTK
    /// application loop returns.
    ///
    /// This registers the preconfigured application through
    /// `RelmApp::from_app`, but drives GTK through a native-argument
    /// `GApplication` bridge rather than `RelmApp::run`.
    /// Consequently, roots must complete typed user-visible shutdown while the
    /// GTK loop is active instead of relying on `RelmApp::run`'s late
    /// initialization or global shutdown sequence. Once the loop returns,
    /// AppShell destroys the root window, when present, and drops the retained
    /// controller. That drop only queues Relm4's shutdown event; without a
    /// final main-context iteration, `Component::shutdown` is not guaranteed to
    /// run after the loop and must not own critical cleanup.
    ///
    /// GTK owns last-window lifetime. A foreign-window coordinator retains its
    /// application privately and reports activations as handled; AppShell
    /// exposes no windowless runner or presentation adapter trait.
    #[must_use]
    pub fn run<C>(spec: AppShellSpec) -> glib::ExitCode
    where
        C: AppShellComponent + 'static,
        C::Root: IsA<glib::Object>,
    {
        run_root_with_args::<C, OsString>(spec, process_args())
    }
}

fn run_root_with_args<C, S>(spec: AppShellSpec, args: Vec<S>) -> glib::ExitCode
where
    C: AppShellComponent + 'static,
    C::Root: IsA<glib::Object>,
    S: AsRef<OsStr>,
{
    // Before GTK is touched, and before any thread exists: the input-method
    // module is read during toolkit initialization and cannot be changed after.
    configure_wayland_input_method();
    // Before anything can panic: every app in the suite gets the same record, so a
    // crash leaves a trace without each app having to remember to ask for one. See
    // `crash_marker` for why a panic in a GTK callback is fatal rather than
    // recoverable, and why the daemons already wrap their work and no GUI did.
    crate::crash_marker::install(spec.application_id());
    // Before the early answers, which print in the product's language.
    if let Some(gettext) = &spec.gettext
        && let Err(error) = gettext.bind()
    {
        log::warn!("app shell: gettext init failed: {error}");
    }
    if let Some(exit_code) = run_early_cli(&spec, &args) {
        return exit_code;
    }
    // Same reasoning, same place: these processes run for months, and every one
    // of them should hand memory back when the system says it needs it rather
    // than each app remembering to ask. Silent when the service manager offers
    // no pressure protocol, which is what a launch from a terminal looks like.
    let _session_runtime = crate::session_runtime::SessionRuntime::acquire();
    leave_out_silent_portal(spec.application_id());
    let watch_dark_mode = spec.watch_dark_mode;
    let Some(app) = boot_application(&spec) else {
        return glib::ExitCode::FAILURE;
    };
    let _relm_app = relm4::RelmApp::<()>::from_app(app.clone());
    let (root, deferred_local_command_line) = connect_root_activation::<C>(&app, watch_dark_mode);
    let application_exit_code = run_application_with_os_args(app.upcast_ref(), &args);
    let exit_code = deferred_local_command_line
        .borrow_mut()
        .take()
        .map_or(application_exit_code, |command_line| {
            command_line.exit_code()
        });
    let retained_root = root.borrow_mut().take();
    if let Some(root) = retained_root {
        if let Some(window) = root.primary_window() {
            window.destroy();
        }
        drop(root);
    }
    exit_code
}

/// Give GTK an input-method module on Wayland when the session left it empty.
///
/// GTK4 composes dead keys through an input-method module. With none set and no
/// input-method daemon running, a Wayland session composes nothing: `´`+`a`
/// types `a`. `simple` is the module that performs the composition.
///
/// Only the gap is filled. A person who set `GTK_IM_MODULE` has already chosen,
/// and so has a session running fcitx5 or ibus, which configure the variable
/// themselves, sometimes after this point. Their choice stands.
///
/// big-compositor composes dead keys itself and sends the text through
/// text-input, which is also how the on-screen keyboard reaches a field;
/// `simple` would not enable text-input there.
fn configure_wayland_input_method() {
    let session_type = std::env::var_os("XDG_SESSION_TYPE");
    let desktop = std::env::var_os("XDG_CURRENT_DESKTOP");
    let configured = std::env::var_os("GTK_IM_MODULE");
    if !needs_simple_input_method(
        session_type.as_deref(),
        desktop.as_deref(),
        configured.as_deref(),
        input_method_daemon_is_running,
    ) {
        return;
    }
    // SAFETY: the first thing `run_root_with_args` does, before GTK
    // initialization and before this process spawns a thread.
    unsafe { std::env::set_var("GTK_IM_MODULE", "simple") };
}

/// How long the desktop portal gets to answer each question before this
/// process starts without it. Starting it with its backends takes about 0.1 s.
const PORTAL_WAIT_MS: i32 = 5000;

/// Start GTK and libadwaita without the desktop portal when it holds its name
/// and does not answer.
///
/// Both ask the portal for versions and settings while they start, one
/// question at a time and each waiting up to 25 s: minutes before the first
/// window, past a session unit's start timeout. The two questions here are a
/// property, which xdg-desktop-portal answers on its main thread, and a
/// setting, which it asks its backend for. Without the portal GTK and
/// libadwaita read GSettings themselves; a portal that answers, even with an
/// error, or is not installed, stays theirs. A launch that hands its arguments
/// to the instance owning `application_id` starts no toolkit and asks nothing.
fn leave_out_silent_portal(application_id: &str) {
    const PORTAL: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return;
    };
    let running = bus
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&(application_id,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
        )
        .ok()
        .and_then(|reply| reply.get::<(bool,)>());
    if running == Some((true,)) {
        return;
    }
    let silent = |interface: &str, method: &str, arguments: glib::Variant| {
        bus.call_sync(
            Some(PORTAL),
            PATH,
            interface,
            method,
            Some(&arguments),
            None,
            gio::DBusCallFlags::NONE,
            PORTAL_WAIT_MS,
            gio::Cancellable::NONE,
        )
        .is_err_and(|error| error.matches(gio::IOErrorEnum::TimedOut))
    };
    let property = ("org.freedesktop.portal.Settings", "version").to_variant();
    let setting = ("org.freedesktop.appearance", "color-scheme").to_variant();
    if !silent("org.freedesktop.DBus.Properties", "Get", property)
        && !silent("org.freedesktop.portal.Settings", "Read", setting)
    {
        return;
    }
    log::warn!(
        "the desktop portal did not answer in {} s: this process starts without it",
        PORTAL_WAIT_MS / 1000
    );
    gtk::disable_portals();
    // SAFETY: glibc 2.41 and later never free an environment array they
    // published (big-host and bigshell depend on that version), so a C
    // `getenv` on the bus thread reads the old array or the new one; Rust
    // readers take std's environment lock.
    unsafe { std::env::set_var("ADW_DISABLE_PORTAL", "1") };
}

/// The whole decision, with the daemon probe left to the caller so it is only
/// paid for when the two cheap answers did not already settle it.
fn needs_simple_input_method(
    session_type: Option<&OsStr>,
    desktop: Option<&OsStr>,
    configured: Option<&OsStr>,
    daemon_is_running: impl Fn() -> bool,
) -> bool {
    let on_wayland = session_type
        .and_then(OsStr::to_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("wayland"));
    let on_big_compositor = desktop
        .and_then(OsStr::to_str)
        .is_some_and(big_os_kit::compositor_session::names_big_compositor);
    if !on_wayland || on_big_compositor || configured.is_some_and(|value| !value.is_empty()) {
        return false;
    }
    !daemon_is_running()
}

/// Whether fcitx5 or ibus is already running for this user.
///
/// Read from `/proc` rather than by spawning `pgrep`: two extra processes at
/// every application start buy nothing, and the exact-name comparison is the
/// same one `pgrep -x` makes.
fn input_method_daemon_is_running() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        let name = entry.file_name();
        if !name
            .to_string_lossy()
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        {
            return false;
        }
        std::fs::read_to_string(entry.path().join("comm"))
            .is_ok_and(|comm| matches!(comm.trim_end(), "fcitx5" | "ibus-daemon"))
    })
}

fn run_early_cli<S: AsRef<OsStr>>(spec: &AppShellSpec, args: &[S]) -> Option<glib::ExitCode> {
    let handler = spec.early_cli?;
    let arguments = args
        .iter()
        .map(|argument| argument.as_ref().to_os_string())
        .collect::<Vec<_>>();
    handler(&arguments)
}

fn run_application_with_os_args<S: AsRef<OsStr>>(
    app: &gio::Application,
    args: &[S],
) -> glib::ExitCode {
    let argv = args.iter().map(AsRef::as_ref).collect::<Vec<&OsStr>>();
    let Ok(argc) = i32::try_from(argv.len()) else {
        log::error!("app shell: argument count exceeds GApplication's integer boundary");
        return glib::ExitCode::FAILURE;
    };
    // SAFETY: `app` remains alive for the complete call. `to_glib_none()`
    // retains every native argument buffer and the null-terminated pointer
    // array until `g_application_run()` returns; `argc` was range-checked.
    let exit_status =
        unsafe { gio::ffi::g_application_run(app.to_glib_none().0, argc, argv.to_glib_none().0) };
    glib::ExitCode::try_from(exit_status).unwrap_or(glib::ExitCode::FAILURE)
}

fn boot_application(spec: &AppShellSpec) -> Option<adw::Application> {
    if let Err(error) = spec.validate() {
        log::error!("app shell: spec invalid: {error}");
        return None;
    }

    let app = adw::Application::new(
        Some(&spec.application_id),
        gio::ApplicationFlags::HANDLES_COMMAND_LINE,
    );
    read_icon_theme_once(&app);
    // GTK and libadwaita start in `startup`, which only the process that owns
    // the application id runs. A launch that hands its arguments to a running
    // instance never opens the display, which saves it the toolkit's start
    // before it exits.
    let (resource_paths, base_css) = (spec.resource_paths, spec.base_css);
    app.connect_startup(move |_| {
        register_resource_paths(resource_paths);
        if let Some(css) = base_css {
            big_relm4_components::css::install(css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
        // Every app in the suite draws its tooltips as the same card.
        big_gtk_kit::install();
        install_focus_ring();
        turn_switches_without_frames();
    });
    Some(app)
}

/// Let a switch change at once in a window that draws no frames.
///
/// `GtkSwitch` changes its state on the last frame of its slide, and a window
/// the compositor suspended (screens off, covered) gets no frames: a key or a
/// screen reader's action would wait until the window is seen again. The hook
/// runs after the switch's own handler has started the slide.
fn turn_switches_without_frames() {
    thread_local! {
        static INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if INSTALLED.replace(true) {
        return;
    }
    let switch = gtk::Switch::static_type();
    let _class = glib::Class::<gtk::Switch>::from_type(switch);
    let Some(activate) = glib::subclass::SignalId::lookup("activate", switch) else {
        log::warn!("app shell: GtkSwitch has no activate signal");
        return;
    };
    // SAFETY: `turn_now` matches `GSignalEmissionHook` and keeps no data.
    unsafe {
        glib::gobject_ffi::g_signal_add_emission_hook(
            activate.into_glib(),
            0,
            Some(turn_now),
            std::ptr::null_mut(),
            None,
        );
    }
}

unsafe extern "C" fn turn_now(
    _hint: *mut glib::gobject_ffi::GSignalInvocationHint,
    n_params: std::ffi::c_uint,
    params: *const glib::gobject_ffi::GValue,
    _data: glib::ffi::gpointer,
) -> glib::ffi::gboolean {
    if n_params == 0 || params.is_null() {
        return glib::ffi::GTRUE;
    }
    // SAFETY: GLib passes the emitting instance first, alive for the call;
    // `glib::Value` is a transparent `GValue`.
    let instance = unsafe { &*params.cast::<glib::Value>() };
    if let Ok(switch) = instance.get::<gtk::Switch>()
        && switch.settings().is_gtk_enable_animations()
        && draws_no_frames(switch.upcast_ref())
    {
        big_gtk_kit::switch::set_active(&switch, !switch.is_active());
    }
    glib::ffi::GTRUE
}

/// Whether `widget` is off screen or in a window the compositor suspended.
fn draws_no_frames(widget: &gtk::Widget) -> bool {
    !widget.is_mapped()
        || widget
            .root()
            .and_then(|root| root.surface())
            .and_downcast::<gtk::gdk::Toplevel>()
            .is_some_and(|toplevel| {
                toplevel
                    .state()
                    .contains(gtk::gdk::ToplevelState::SUSPENDED)
            })
}

/// Let GTK read the icon theme once, on its own thread.
///
/// The display's icon theme starts its read on a thread when it is created,
/// and a resource path added after that throws the read away: the next lookup
/// reads the whole theme again on the main thread. `startup` adds two,
/// GtkApplication's `<resource base path>/icons/` and libadwaita's own. So
/// libadwaita starts as soon as the display opens, which adds its path right
/// as it creates the theme, and the base path is dropped: no program of the
/// suite ships icons, menus or a help overlay under it.
fn read_icon_theme_once(app: &adw::Application) {
    app.set_resource_base_path(None);
    // The display opens inside `startup`, after GtkApplication has given the
    // portals its id and before it looks at the icon theme. GDK aborts when
    // its display manager is asked for before GTK starts, so the signal is
    // caught on the class.
    let manager = gtk::gdk::DisplayManager::static_type();
    let _class = glib::Class::<gtk::gdk::DisplayManager>::from_type(manager);
    let Some(opened) = glib::subclass::SignalId::lookup("display-opened", manager) else {
        log::warn!("app shell: GDK has no display-opened signal");
        return;
    };
    // SAFETY: `start_adwaita` matches `GSignalEmissionHook` and keeps no data.
    unsafe {
        glib::gobject_ffi::g_signal_add_emission_hook(
            opened.into_glib(),
            0,
            Some(start_adwaita),
            std::ptr::null_mut(),
            None,
        );
    }
}

unsafe extern "C" fn start_adwaita(
    _hint: *mut glib::gobject_ffi::GSignalInvocationHint,
    _n_params: std::ffi::c_uint,
    _params: *const glib::gobject_ffi::GValue,
    _data: glib::ffi::gpointer,
) -> glib::ffi::gboolean {
    if let Err(error) = adw::init() {
        log::warn!("app shell: libadwaita did not start with the display: {error}");
    }
    // Once: the hook is removed.
    glib::ffi::GFALSE
}

fn register_resource_paths(paths: &[&str]) {
    if paths.is_empty() {
        return;
    }
    let Some(display) = gtk::gdk::Display::default() else {
        log::warn!(
            "app shell: no default display; skipping {} resource path(s)",
            paths.len()
        );
        return;
    };
    let icon_theme = gtk::IconTheme::for_display(&display);
    for path in paths {
        icon_theme.add_resource_path(path);
    }
}

/// Draw every focus ring in the whole accent, for the suite's apps.
///
/// libadwaita mixes the accent half with transparency, which measures under the
/// 3:1 a focus indicator needs against the window (WCAG 1.4.11).
pub fn install_focus_ring() {
    big_relm4_components::css::install(
        "*:focus:focus-visible, entry:focus-within { outline-color: var(--accent-color); }",
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

type RootControllerSlot<C> = Rc<RefCell<Option<AppShellController<C>>>>;
type DeferredLocalCommandLineSlot = Rc<RefCell<Option<gio::ApplicationCommandLine>>>;

fn connect_root_activation<C>(
    app: &adw::Application,
    watch_dark_mode: bool,
) -> (RootControllerSlot<C>, DeferredLocalCommandLineSlot)
where
    C: AppShellComponent + 'static,
    C::Root: IsA<glib::Object>,
{
    let root = Rc::new(RefCell::new(None::<AppShellController<C>>));
    let root_for_activation = Rc::clone(&root);
    let deferred_local_command_line = Rc::new(RefCell::new(None));
    let deferred_local_command_line_for_activation = Rc::clone(&deferred_local_command_line);
    app.connect_command_line(move |app, command_line| {
        let activation = AppShellActivation::from_command_line(command_line);
        let (request, tracker) = AppShellActivationRequest::new(activation);
        tracker.arm();
        let dispatch = std::panic::catch_unwind(AssertUnwindSafe(|| {
            drive_root_activation::<C>(app, &root_for_activation, request)
        }));
        match dispatch {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracker.fail_before_defer(error),
            Err(_) => {
                tracker.fail_before_defer(AppShellActivationError::new(ACTIVATION_PANIC_ERROR, 1))
            }
        }
        match tracker.resolve_or_defer(app.upcast_ref(), command_line) {
            Some(Ok(())) => glib::ExitCode::SUCCESS,
            Some(Err(error)) => {
                print_command_line_error(command_line, error.message());
                glib::ExitCode::from(error.status())
            }
            None => {
                if !command_line.is_remote()
                    && deferred_local_command_line_for_activation
                        .borrow()
                        .is_none()
                {
                    *deferred_local_command_line_for_activation.borrow_mut() =
                        Some(command_line.clone());
                }
                glib::ExitCode::SUCCESS
            }
        }
    });
    if watch_dark_mode {
        let root_for_dark_mode = Rc::clone(&root);
        app.connect_startup(move |_| {
            let root = Rc::clone(&root_for_dark_mode);
            // leak-guard: allow(process-object-handler) AppShell wires one root per process
            adw::StyleManager::default().connect_dark_notify(move |style_manager| {
                let Some(input) = C::input_for_dark_mode(style_manager.is_dark()) else {
                    return;
                };
                if let Some(root) = root.borrow().as_ref() {
                    let _ = root.emit(input);
                }
            });
        });
    }
    (root, deferred_local_command_line)
}

fn drive_root_activation<C>(
    app: &adw::Application,
    root_slot: &Rc<RefCell<Option<AppShellController<C>>>>,
    request: AppShellActivationRequest,
) -> Result<(), AppShellActivationError>
where
    C: AppShellComponent + 'static,
    C::Root: IsA<glib::Object>,
{
    let outcome = C::activation_outcome(request.activation());
    if root_slot.borrow().is_none() {
        let context = AppShellContext::new(app);
        let init = C::init_for_activation(&context, request)?;
        let root = AppShellController::<C>::launch(init);
        root.bind_primary_window(app);
        let window = root.window_for_presentation(app, outcome, false)?;
        *root_slot.borrow_mut() = Some(root);
        if let Some(window) = window {
            log_active_renderer_once(&window);
            window.present();
        }
        return Ok(());
    }

    let input = C::input_for_activation(request)?;
    if let Some(root) = root_slot.borrow().as_ref() {
        let window = root.window_for_presentation(app, outcome, true)?;
        root.emit(input)?;
        if let Some(window) = window {
            window.present();
        }
    }
    Ok(())
}

/// Log which GSK renderer the display actually chose, once, when the first
/// window maps.
///
/// The renderer decides whether every window's buffer reaches the compositor as
/// a GPU dmabuf (`gl`/`vulkan`, imported zero-copy) or as a per-frame software
/// upload (`cairo`). A silent fall to `cairo` on an old GPU is otherwise
/// invisible, and it is the dominant per-frame CPU cost on a machine like the
/// i3; this one line makes it show up in the log.
fn log_active_renderer_once(window: &gtk::Window) {
    use std::sync::atomic::{AtomicBool, Ordering};

    static LOGGED: AtomicBool = AtomicBool::new(false);
    window.connect_map(move |window| {
        if LOGGED.swap(true, Ordering::Relaxed) {
            return;
        }
        let renderer = window
            .renderer()
            .map_or_else(|| "unknown".to_owned(), |r| r.type_().name().to_string());
        let pinned =
            std::env::var("GSK_RENDERER").unwrap_or_else(|_| "unset (GTK default)".to_owned());
        log::info!("GSK renderer in use: {renderer} (GSK_RENDERER={pinned})");
    });
}

fn process_args() -> Vec<OsString> {
    std::env::args_os().collect()
}

fn require(condition: bool, error: AppShellSpecError) -> Result<(), AppShellSpecError> {
    if condition { Ok(()) } else { Err(error) }
}

#[cfg(test)]
mod tests;
