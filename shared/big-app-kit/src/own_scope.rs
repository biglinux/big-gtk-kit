// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Programs a person starts from one of ours run in a systemd scope of their own.
//!
//! A child forked by the host lands in the host's cgroup, where its memory counts
//! as the desktop's and restarting the desktop kills it. Every launch goes
//! through here: [`open_uri`] and [`launch`] for what GIO starts,
//! [`launch_in_terminal`] for a command line run in the desktop's terminal,
//! [`move_to_own_scope`] for a program spawned by argv or launched with a
//! context of the caller's own. D-Bus-activated applications already run in
//! their own scope and report no pid.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

/// Open `uri` (a web address, a file's URI, `trash:///`) with the program this
/// desktop opens it with, allowed to come to the front, in a scope of its own.
///
/// `token` is an activation token minted for the input that asked, as the
/// compositor mints for a press it handles itself; without one, call it from
/// the handler of the click or key press that asked for it: the token is
/// minted then, for that input. Asynchronous because the handler is found by
/// reading the thing: a file's type is sniffed from its first bytes and a
/// remote URI asks gvfs.
///
/// # Errors
///
/// No program handles `uri`, or it could not be started.
pub async fn open_uri(uri: &str, token: Option<&str>) -> Result<(), glib::Error> {
    let context = launch_context(Renderer::DefaultGpu, token, None);
    // As in `launch`: a D-Bus-activated handler would not get the token.
    if token.is_some() {
        let handler = gio::File::for_uri(uri)
            .query_default_handler_future(glib::Priority::DEFAULT)
            .await?;
        // The caller's string, as GIO passes it: a URI made into a GFile and
        // back loses parts of it (`mailto:` gains slashes, a fragment goes).
        return spawned(&handler).as_ref().unwrap_or(&handler).launch_uris(
            &[uri],
            context
                .as_ref()
                .map(|context| context.upcast_ref::<gio::AppLaunchContext>()),
        );
    }
    gio::AppInfo::launch_default_for_uri_future(uri, context.as_ref()).await
}

/// Whether the desktop's portal can ask the person which program opens a file.
///
/// `gtk::FileLauncher::set_always_ask` asks only through that portal; without
/// one GTK opens the file with its default program instead of asking, so a
/// caller offering a choice checks first and says why there is none.
pub async fn can_ask_for_program() -> bool {
    let Ok(bus) = gio::bus_get_future(gio::BusType::Session).await else {
        return false;
    };
    bus.call_future(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.DBus.Properties",
        "Get",
        Some(&("org.freedesktop.portal.OpenURI", "version").to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        -1,
    )
    .await
    .is_ok()
}

/// Start `application` with `files`, allowed to come to the front, in a scope of
/// its own. Same `token` rule as [`open_uri`]. `started` is told the process of
/// each program it starts: none for a program the session bus starts.
///
/// A desktop entry with `PrefersNonDefaultGPU=true` (games, Steam) starts on the
/// GPU that does not drive the screens, as [`launch_on`] with
/// [`Renderer::NonDefaultGpu`] does.
///
/// While this process asks programs for their accessibility tree
/// (`GNOME_ACCESSIBILITY=1`), a Chromium-family browser starts with the flag that
/// makes it build the tree of web pages too.
///
/// # Errors
///
/// The program could not be started.
pub fn launch(
    application: &gio::AppInfo,
    files: &[gio::File],
    token: Option<&str>,
    started: Option<Started>,
    failed: Option<Failed>,
) -> Result<(), glib::Error> {
    if prefers_non_default_gpu(application) {
        return launch_on(
            application,
            files,
            Renderer::NonDefaultGpu,
            token,
            started,
            failed,
        );
    }
    let accessible = with_page_accessibility(application);
    let application = accessible.as_ref().unwrap_or(application);
    let context = launch_context(Renderer::DefaultGpu, token, started);
    // A given token travels in the context's environment, which D-Bus
    // activation does not pass on: such an entry is started by its command.
    if token.is_some() {
        return start(application, files, context.as_ref());
    }
    application.launch(files, context.as_ref())
}

/// Chromium-family browsers, by the program their entry runs. They build a web
/// page's accessibility tree only when started with `--force-renderer-accessibility`,
/// whatever the environment and `org.a11y.Status` say.
const CHROMIUM_FAMILY: &[&str] = &[
    "chromium",
    "chromium-browser",
    "google-chrome",
    "google-chrome-stable",
    "brave",
    "brave-browser",
    "vivaldi-stable",
    "microsoft-edge-stable",
    "opera",
];

/// `application` started with `--force-renderer-accessibility` when it is a
/// Chromium-family browser and this process asks programs for their
/// accessibility tree (`GNOME_ACCESSIBILITY=1`). A browser that is already
/// running keeps the flags it started with. The copy has no desktop id.
///
/// Reads the entry on the calling thread, as [`prefers_non_default_gpu`] does:
/// [`launch`] answers whether the program started, and the shell tells the
/// person when it did not.
fn with_page_accessibility(application: &gio::AppInfo) -> Option<gio::AppInfo> {
    std::env::var_os("GNOME_ACCESSIBILITY").filter(|value| value == "1")?;
    // Parsed from `Exec`: GIO's executable keeps the quotes of a quoted
    // program, and an entry without `Exec` has none.
    let argv = glib::shell_parse_argv(application.commandline()?).ok()?;
    let program = std::path::Path::new(argv.first()?);
    if !CHROMIUM_FAMILY.contains(&program.file_name()?.to_str()?) {
        return None;
    }
    forcing_page_accessibility(&entry(application)?)
}

/// The program of the entry `keys` holds, `--force-renderer-accessibility` after
/// its own name.
fn forcing_page_accessibility(keys: &glib::KeyFile) -> Option<gio::AppInfo> {
    let exec = keys.string("Desktop Entry", "Exec").ok()?;
    // The program is the first argument, quoted when it holds a space; the field
    // codes after it stay unquoted for GIO to expand.
    let end = match exec.strip_prefix('"') {
        Some(quoted) => quoted.find('"')? + 2,
        None => exec.find(' ').unwrap_or(exec.len()),
    };
    let (program, arguments) = exec.split_at(end);
    keys.set_string(
        "Desktop Entry",
        "Exec",
        &format!("{program} --force-renderer-accessibility{arguments}"),
    );
    big_relm4_components::desktop_entry::app_info_from_keyfile(keys)
}

/// Start the desktop entry `Exec` value `exec` in the desktop's terminal, in a
/// scope of its own.
///
/// Started on a GIO worker: GIO looks for the terminal on `PATH` and forks this
/// process, which the GTK thread should not wait on. An entry made from a command
/// line has no `StartupNotify`, so GIO asks no display for an activation token
/// and a plain launch context loses nothing.
///
/// # Errors
///
/// `exec` is not a valid `Exec` value, or the terminal could not be started.
pub async fn launch_in_terminal(exec: String) -> Result<(), glib::Error> {
    gio::spawn_blocking(move || {
        let application = gio::AppInfo::create_from_commandline(
            exec,
            None,
            gio::AppInfoCreateFlags::NEEDS_TERMINAL,
        )?;
        let context = gio::AppLaunchContext::new();
        set_environment(&context, Renderer::DefaultGpu);
        rehome(&context, None);
        application.launch(&[], Some(&context))
    })
    .await
    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// What a program the desktop starts draws with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Renderer {
    /// The GPU that drives the screens: the integrated one of a hybrid laptop.
    /// [`launch`] uses it, without asking switcheroo-control, for an entry that
    /// prefers no GPU.
    DefaultGpu,
    /// The other GPU: the dedicated one of a hybrid laptop.
    NonDefaultGpu,
    /// The CPU, for a program that a video driver fails.
    Software,
}

/// Mesa's llvmpipe for OpenGL and EGL, the Mesa vendor for GLX and EGL so
/// glvnd loads no NVIDIA library, lavapipe as the only Vulkan driver, and the
/// CPU renderers of GTK 4 and Qt Quick. Chromium and Electron draw through the
/// system's EGL and GL, so they follow without an argument of their own.
///
/// `flatpak run` keeps `__EGL_VENDOR_LIBRARY_FILENAMES` out of the sandbox,
/// where a host path would name nothing anyway; `FLATPAK_GL_DRIVERS` mounts
/// the runtime's Mesa alone, without the NVIDIA GL extension.
const SOFTWARE_RENDERING: [(&str, &str); 8] = [
    ("LIBGL_ALWAYS_SOFTWARE", "1"),
    ("GALLIUM_DRIVER", "llvmpipe"),
    ("__GLX_VENDOR_LIBRARY_NAME", "mesa"),
    (
        "__EGL_VENDOR_LIBRARY_FILENAMES",
        "/usr/share/glvnd/egl_vendor.d/50_mesa.json",
    ),
    ("FLATPAK_GL_DRIVERS", "default"),
    ("VK_LOADER_DRIVERS_SELECT", "*lvp*"),
    ("GSK_RENDERER", "cairo"),
    ("QT_QUICK_BACKEND", "software"),
];

/// [`launch`] drawing with `renderer`, whatever the desktop entry prefers. The
/// command line is the entry's own for every renderer, with [`launch`]'s
/// accessibility flag.
///
/// A GPU takes the variables switcheroo-control names for it, which also keep
/// a Vulkan program that picks the dedicated GPU by itself (DXVK) on the
/// integrated one; the non-default GPU is turned on first where
/// `big-dgpu-power` has it off. That asks the system bus first; a failure of
/// that lookup shows in the log only, and the program starts as [`launch`]
/// would start an entry without a preference. Such a launch answers before
/// the program starts: `failed` hears why it could not.
///
/// # Errors
///
/// The program could not be started.
pub fn launch_on(
    application: &gio::AppInfo,
    files: &[gio::File],
    renderer: Renderer,
    token: Option<&str>,
    started: Option<Started>,
    failed: Option<Failed>,
) -> Result<(), glib::Error> {
    let application = &with_page_accessibility(application).unwrap_or_else(|| application.clone());
    let context = launch_context(renderer, token, started);
    if renderer != Renderer::Software {
        let application = application.clone();
        let files = files.to_vec();
        let non_default = renderer == Renderer::NonDefaultGpu;
        glib::spawn_future_local(async move {
            let environment = gpu_environment(non_default).await;
            if let Some(context) = &context {
                set_gpu_environment(context, non_default, &environment.unwrap_or_default());
            }
            if let Err(error) = start(&application, &files, context.as_ref()) {
                log::warn!("cannot start {}: {error}", application.display_name());
                if let Some(failed) = failed {
                    failed(error);
                }
            }
        });
        return Ok(());
    }
    start(application, files, context.as_ref())
}

/// Start `application` with `context`'s environment.
///
/// GIO starts a `DBusActivatable` entry by asking the session bus, whose service
/// runs with the bus's environment, so such an entry is started from its keys,
/// which GIO spawns.
fn start(
    application: &gio::AppInfo,
    files: &[gio::File],
    context: Option<&impl IsA<gio::AppLaunchContext>>,
) -> Result<(), glib::Error> {
    spawned(application)
        .as_ref()
        .unwrap_or(application)
        .launch(files, context.map(|context| context.upcast_ref()))
}

/// The copy of a `DBusActivatable` `application` GIO spawns from its keys.
fn spawned(application: &gio::AppInfo) -> Option<gio::AppInfo> {
    entry(application)
        .filter(|keys| keys.boolean("Desktop Entry", "DBusActivatable") == Ok(true))
        .and_then(|keys| big_relm4_components::desktop_entry::app_info_from_keyfile(&keys))
}

/// Whether a program can start on a GPU other than the one that drives the
/// screens: switcheroo-control has two, or `big-dgpu-power` runs, which may
/// hold the NVIDIA GPU off the bus where switcheroo-control cannot see it.
pub async fn has_non_default_gpu() -> bool {
    let Ok(bus) = gio::bus_get_future(gio::BusType::System).await else {
        return false;
    };
    let dual = bus
        .call_future(
            Some("net.hadess.SwitcherooControl"),
            "/net/hadess/SwitcherooControl",
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&("net.hadess.SwitcherooControl", "HasDualGpu").to_variant()),
            Some(&glib::VariantType::new_tuple([glib::VariantTy::VARIANT])),
            gio::DBusCallFlags::NONE,
            2000,
        )
        .await
        .ok()
        .and_then(|reply| reply.child_value(0).as_variant())
        .and_then(|dual| dual.get::<bool>());
    if dual == Some(true) {
        return true;
    }
    bus.call_future(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&("br.com.biglinux.DiscreteGpu1",).to_variant()),
        Some(&glib::VariantType::new_tuple([glib::VariantTy::BOOLEAN])),
        gio::DBusCallFlags::NONE,
        2000,
    )
    .await
    .ok()
    .and_then(|reply| reply.child_value(0).get::<bool>())
    .unwrap_or(false)
}

/// Whether `application`'s desktop entry says `PrefersNonDefaultGPU=true`.
#[must_use]
pub fn prefers_non_default_gpu(application: &gio::AppInfo) -> bool {
    entry(application)
        .is_some_and(|keys| keys.boolean("Desktop Entry", "PrefersNonDefaultGPU") == Ok(true))
}

/// Whether `application`'s desktop entry says `SingleMainWindow=true`: starting
/// it again while that window is open only raises it.
pub fn single_main_window(application: &gio::AppInfo) -> bool {
    entry(application)
        .is_some_and(|keys| keys.boolean("Desktop Entry", "SingleMainWindow") == Ok(true))
}

/// The desktop file `application` was read from, read again.
fn entry(application: &gio::AppInfo) -> Option<glib::KeyFile> {
    // gio 0.22 binds no `GDesktopAppInfo`; its "filename" property is the entry.
    let path = application.property::<Option<String>>("filename")?;
    let keys = glib::KeyFile::new();
    keys.load_from_file(path, glib::KeyFileFlags::NONE).ok()?;
    Some(keys)
}

/// The environment switcheroo-control gives for the first GPU that is not the
/// default, or for the default one; `None` without such a GPU or the service.
async fn gpu_environment(non_default: bool) -> Option<Vec<(String, String)>> {
    let bus = gio::bus_get_future(gio::BusType::System).await.ok()?;
    if non_default {
        wake_discrete_gpu(&bus).await;
    }
    let reply = bus
        .call_future(
            Some("net.hadess.SwitcherooControl"),
            "/net/hadess/SwitcherooControl",
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&("net.hadess.SwitcherooControl", "GPUs").to_variant()),
            Some(&glib::VariantType::new_tuple([glib::VariantTy::VARIANT])),
            gio::DBusCallFlags::NONE,
            2000,
        )
        .await
        .map_err(|error| log::warn!("switcheroo-control did not answer: {error}"))
        .ok()?;
    environment_of(&reply.child_value(0).as_variant()?, !non_default)
}

/// Ask BigLinux's `big-dgpu-power` to turn a hybrid laptop's NVIDIA GPU on, and
/// wait until it is: while that service has it off, the GPU is not on the PCI
/// bus and switcheroo-control does not list it. The service runs for as long as
/// the GPU is off, so one that is not running is not started: the GPU is on.
async fn wake_discrete_gpu(bus: &gio::DBusConnection) {
    let woken = bus
        .call_future(
            Some("br.com.biglinux.DiscreteGpu1"),
            "/br/com/biglinux/DiscreteGpu1",
            "br.com.biglinux.DiscreteGpu1",
            "Wake",
            None,
            None,
            gio::DBusCallFlags::NO_AUTO_START,
            // The service may first finish powering the GPU off; each way, modprobe
            // may take thirty seconds and the driver ten more to have it back.
            120_000,
        )
        .await;
    if let Err(error) = woken
        && !error.matches(gio::DBusError::ServiceUnknown)
        && !error.matches(gio::DBusError::NameHasNoOwner)
        && !error.matches(gio::DBusError::NotSupported)
    {
        log::warn!("the NVIDIA GPU was not turned on: {error}");
    }
}

/// From switcheroo-control's `GPUs` (`aa{sv}`), the `Environment` of the first
/// GPU whose `Default` is `default`, as variable and value pairs.
fn environment_of(gpus: &glib::Variant, default: bool) -> Option<Vec<(String, String)>> {
    let gpus = gpus.get::<Vec<std::collections::HashMap<String, glib::Variant>>>()?;
    let gpu = gpus
        .iter()
        .find(|gpu| gpu.get("Default").and_then(glib::Variant::get::<bool>) == Some(default))?;
    let flat = gpu.get("Environment")?.get::<Vec<String>>()?;
    Some(
        flat.as_chunks::<2>()
            .0
            .iter()
            .map(|[variable, value]| (variable.clone(), value.clone()))
            .collect(),
    )
}

/// A launch context that carries an activation token, gives `renderer`'s
/// environment ([`set_environment`]), and moves what it starts into a scope of
/// its own: `token` in `XDG_ACTIVATION_TOKEN` and `DESKTOP_STARTUP_ID`, or
/// without one the display's context, which mints one at launch for this
/// program's last input. `None` when neither a token nor a display is there.
///
/// The token is what lets a program that is already running come to the front
/// on Wayland; without one it opens behind the window it was asked from.
fn launch_context(
    renderer: Renderer,
    token: Option<&str>,
    started: Option<Started>,
) -> Option<gio::AppLaunchContext> {
    let context = match token {
        Some(token) => {
            let context = gio::AppLaunchContext::new();
            context.setenv("XDG_ACTIVATION_TOKEN", token);
            context.setenv("DESKTOP_STARTUP_ID", token);
            context
        }
        None => gdk::Display::default()?.app_launch_context().upcast(),
    };
    set_environment(&context, renderer);
    rehome(&context, started);
    Some(context)
}

/// Give what `context` starts the person's environment, with the software
/// renderer's variables set last (`__EGL_VENDOR_LIBRARY_FILENAMES` is in both
/// lists).
fn set_environment(context: &impl IsA<gio::AppLaunchContext>, renderer: Renderer) {
    for (variable, value) in big_os_kit::subprocess::person_environment() {
        match value {
            Some(value) => context.setenv(variable, value),
            None => context.unsetenv(variable),
        }
    }
    if renderer == Renderer::Software {
        set_variables(context, &SOFTWARE_RENDERING);
    }
}

/// Give what `context` starts switcheroo-control's `environment` for the GPU
/// chosen. The integrated one also loses the NVIDIA offload variables a
/// session may export, which would keep GLX and Vulkan on the NVIDIA GPU.
fn set_gpu_environment(
    context: &impl IsA<gio::AppLaunchContext>,
    non_default: bool,
    environment: &[(String, String)],
) {
    if !non_default {
        for variable in [
            "__NV_PRIME_RENDER_OFFLOAD",
            "__GLX_VENDOR_LIBRARY_NAME",
            "__VK_LAYER_NV_optimus",
        ] {
            context.unsetenv(variable);
        }
    }
    set_variables(context, environment);
}

/// Set `variables` on `context`. A Vulkan driver selection among them drops a
/// driver list of the person's own (an `environment.d` tweak): one without the
/// selected driver would leave the loader with no driver at all.
fn set_variables(
    context: &impl IsA<gio::AppLaunchContext>,
    variables: &[(impl AsRef<str>, impl AsRef<str>)],
) {
    if variables
        .iter()
        .any(|(variable, _)| variable.as_ref() == "VK_LOADER_DRIVERS_SELECT")
    {
        context.unsetenv("VK_DRIVER_FILES");
        context.unsetenv("VK_ICD_FILENAMES");
    }
    for (variable, value) in variables {
        context.setenv(variable.as_ref(), value.as_ref());
    }
}

/// What is told the process of each program a launch starts.
pub type Started = Box<dyn Fn(u32)>;

/// What is told why a launch that answered before starting, as one on the
/// dedicated GPU does, could not start its program.
pub type Failed = Box<dyn FnOnce(glib::Error)>;

/// Move whatever `context` spawns into a scope of its own, and tell `started`
/// its process.
///
/// The `launched` signal carries the pid whichever application the launch
/// resolved to, so this covers `launch_default_for_uri` as well as a named one.
fn rehome(context: &impl IsA<gio::AppLaunchContext>, started: Option<Started>) {
    context.connect_launched(move |_, application, platform_data| {
        let Some(pid) = platform_data
            .get::<std::collections::HashMap<String, glib::Variant>>()
            .and_then(|metadata| metadata.get("pid").and_then(glib::Variant::get::<i32>))
            .and_then(|pid| u32::try_from(pid).ok())
        else {
            return;
        };
        // An entry started from its keys has no id; its program names it.
        let name = application.id().map_or_else(
            || {
                application.executable().file_name().map_or_else(
                    || "unknown".to_owned(),
                    |name| name.to_string_lossy().into_owned(),
                )
            },
            |id| id.trim_end_matches(".desktop").to_owned(),
        );
        move_to_own_scope(pid, &name);
        if let Some(started) = &started {
            started(pid);
        }
    });
}

/// Move `pid` into a fresh transient scope on the systemd user manager, named
/// after `name`. A program spawned by argv for the person takes their
/// environment from [`big_os_kit::subprocess::person_environment`].
///
/// Fire-and-forget: on failure the program still runs, only coupled to ours, and
/// a session without systemd only logs a warning. Callable from any thread: the
/// bus call runs on the main context, and the terminal starts its PTY daemon
/// from a worker.
pub fn move_to_own_scope(pid: u32, name: &str) {
    let Some((unit, parameters)) = scope_for(pid, name) else {
        return;
    };
    // Spawned, never invoked: `invoke` runs the call on this thread when it
    // can take the main context, and the bus answer then lands on the main
    // thread holding a callback made on this one.
    glib::MainContext::default().spawn(async move { ask_systemd(unit, parameters) });
}

/// [`move_to_own_scope`] for a process whose main context never runs: waits
/// for systemd's answer on this thread.
///
/// # Errors
///
/// The session bus or systemd refused; the program still runs, in our scope.
pub fn move_to_own_scope_now(pid: u32, name: &str) -> Result<(), glib::Error> {
    let Some((_, parameters)) = scope_for(pid, name) else {
        return Ok(());
    };
    gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?.call_sync(
        Some("org.freedesktop.systemd1"),
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
        "StartTransientUnit",
        Some(&parameters),
        Some(&reply_type()),
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
    )?;
    Ok(())
}

/// The scope unit for `pid` and the `StartTransientUnit` call that makes it;
/// nothing for a pid that is not another program.
fn scope_for(pid: u32, name: &str) -> Option<(String, glib::Variant)> {
    // systemd reads PID 0 as "the caller", which would move this whole
    // process into the program's scope; our own pid would do the same.
    if pid == 0 || pid == std::process::id() {
        return None;
    }
    big_os_kit::subprocess::restore_limits(pid);
    let mut stem: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if stem.is_empty() {
        stem.push('_');
    }
    let unit = format!("app-bigshell-{stem}-{pid}.scope");
    let parameters = scope_request(&unit, pid, &stem);
    Some((unit, parameters))
}

/// `StartTransientUnit` answers with the job's object path.
fn reply_type() -> glib::VariantType {
    glib::VariantType::new_tuple([glib::VariantTy::OBJECT_PATH])
}

/// `StartTransientUnit(ssa(sv)a(sa(sv)))` for `pid`'s scope.
fn scope_request(unit: &str, pid: u32, stem: &str) -> glib::Variant {
    let properties = glib::Variant::array_from_iter_with_type(
        &<(String, glib::Variant)>::static_variant_type(),
        [
            ("PIDs".to_owned(), vec![pid].to_variant()).to_variant(),
            (
                "Description".to_owned(),
                format!("Application {stem}").to_variant(),
            )
                .to_variant(),
            // Collect failed scopes too: a program that crashed must not leave a
            // failed unit behind on the user manager.
            ("CollectMode".to_owned(), "inactive-or-failed".to_variant()).to_variant(),
            // Stopped at logout: the user manager outlives the session (ssh,
            // linger), and so would every program the desktop started.
            (
                "PartOf".to_owned(),
                vec!["graphical-session.target".to_owned()].to_variant(),
            )
                .to_variant(),
            // A starting program reads most of what it needs: the disks of `/`
            // and `/home` answer it within 2 ms before the units beside it,
            // until it loses the keyboard and big-compositor relaxes the target.
            (
                "IODeviceLatencyTargetUSec".to_owned(),
                vec![("/".to_owned(), 2_000_u64), ("/home".to_owned(), 2_000_u64)].to_variant(),
            )
                .to_variant(),
        ],
    );
    let aux = glib::Variant::array_from_iter_with_type(
        &<(String, Vec<(String, glib::Variant)>)>::static_variant_type(),
        std::iter::empty::<glib::Variant>(),
    );
    // tuple_from_iter keeps the arrays as they are; a plain tuple `.to_variant()`
    // would box them into "v" ('ssvv' is not 'ssa(sv)a(sa(sv))').
    glib::Variant::tuple_from_iter([unit.to_variant(), "fail".to_variant(), properties, aux])
}

fn ask_systemd(unit: String, parameters: glib::Variant) {
    gio::bus_get(gio::BusType::Session, gio::Cancellable::NONE, move |bus| {
        let Ok(bus) = bus else {
            log::warn!("session bus unavailable; the program stays in our scope: {unit}");
            return;
        };
        bus.call(
            Some("org.freedesktop.systemd1"),
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
            "StartTransientUnit",
            Some(&parameters),
            Some(&reply_type()),
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |result| {
                if let Err(error) = result {
                    log::warn!(
                        "scope move failed; the program stays in our scope: {unit}: {error}"
                    );
                }
            },
        );
    });
}

#[cfg(test)]
#[path = "own_scope_tests.rs"]
mod tests;
