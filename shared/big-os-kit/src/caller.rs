// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Which installed program a caller runs, for a service that answers one
//! program only: read from `/proc/PID/exe`, and kept only while the process a
//! pidfd names still lives, so a reused pid cannot vouch for another program.
//! An installed program started with a variable that loads code into it
//! ([`CODE_LOADING_VARIABLES`]) runs that code too, and vouches for nothing.
//! It names a program, never a permission: a process that sends a call and
//! then executes the installed program passes, so anything privileged stays
//! behind polkit.

use std::os::fd::BorrowedFd;
#[cfg(feature = "bus-caller")]
use std::os::unix::ffi::OsStrExt;
#[cfg(feature = "bus-caller")]
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Environment variables that make a program load code from where they point:
/// the dynamic loader's, the C library's (iconv modules), and those of the
/// libraries loaded by the programs these checks vouch for (GLib, GTK,
/// GdkPixbuf, GStreamer, PipeWire, Mesa, EGL, NVIDIA's EGL, Vulkan, VA-API,
/// VDPAU, Qt, Fcitx, Python).
pub const CODE_LOADING_VARIABLES: [&str; 45] = [
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "GCONV_PATH",
    "GIO_MODULE_DIR",
    "GIO_EXTRA_MODULES",
    "GTK_MODULES",
    "GTK3_MODULES",
    "GTK_PATH",
    "GTK_EXE_PREFIX",
    "GTK_IM_MODULE_FILE",
    "GDK_PIXBUF_MODULE_FILE",
    "GDK_PIXBUF_MODULEDIR",
    "GST_PLUGIN_PATH",
    "GST_PLUGIN_PATH_1_0",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    "GST_PLUGIN_SCANNER",
    "GST_PLUGIN_SCANNER_1_0",
    "GST_REGISTRY",
    "GST_REGISTRY_1_0",
    "PIPEWIRE_MODULE_DIR",
    "SPA_PLUGIN_DIR",
    "LIBGL_DRIVERS_PATH",
    "GBM_BACKENDS_PATH",
    "__EGL_VENDOR_LIBRARY_DIRS",
    "__EGL_VENDOR_LIBRARY_FILENAMES",
    "__EGL_EXTERNAL_PLATFORM_CONFIG_DIRS",
    "__EGL_EXTERNAL_PLATFORM_CONFIG_FILENAMES",
    "VK_ICD_FILENAMES",
    "VK_DRIVER_FILES",
    "VK_ADD_DRIVER_FILES",
    "VK_LAYER_PATH",
    "VK_ADD_LAYER_PATH",
    "VK_IMPLICIT_LAYER_PATH",
    "VK_ADD_IMPLICIT_LAYER_PATH",
    "VK_INSTANCE_LAYERS",
    "VK_LOADER_LAYERS_ENABLE",
    "LIBVA_DRIVERS_PATH",
    "VDPAU_DRIVER_PATH",
    "QT_PLUGIN_PATH",
    "QT_QPA_PLATFORM_PLUGIN_PATH",
    "FCITX_ADDON_DIRS",
    "PYTHONPATH",
    "PYTHONHOME",
];

/// The path of the file `running`, read from `/proc/PID/exe`, was started
/// from: after an upgrade the kernel names the replaced file "`path` (deleted)".
pub fn installed_path(running: &Path) -> Option<&str> {
    let running = running.to_str()?;
    Some(running.strip_suffix(" (deleted)").unwrap_or(running))
}

/// Whether `pid` runs the installed program `path`, with nothing its
/// environment loads into it ([`loads_no_foreign_code`]). The caller may
/// have a mount namespace of its own, where `/proc/PID/exe` names whatever
/// file was mounted at `path`, so the file it runs must also be the one at
/// `path` here, or one an upgrade unlinked, owned by root and writable by
/// nobody else. A root-owned program the user mounted over `path` in a user
/// namespace before an upgrade unlinks it passes then: with Yama's
/// `ptrace_scope` at 1 no more than a parent tracing its child can do, at 2
/// or 3 a route of its own.
#[cfg(feature = "bus-caller")]
pub fn runs_installed(pid: u32, path: &str) -> bool {
    let exe = format!("/proc/{pid}/exe");
    std::fs::read_link(&exe).is_ok_and(|running| installed_path(&running) == Some(path))
        && std::fs::metadata(&exe).is_ok_and(|running| {
            std::fs::metadata(path).is_ok_and(|installed| {
                let same = running.dev() == installed.dev() && running.ino() == installed.ino();
                installed_file(running.uid(), running.mode(), same, running.nlink())
            })
        })
        && loads_no_foreign_code(pid)
}

/// Whether the running file is root's, writable by nobody else, and the file
/// at the installed path (`same`) or one an upgrade unlinked (no `links`).
#[cfg(feature = "bus-caller")]
fn installed_file(owner: u32, mode: u32, same: bool, links: u64) -> bool {
    owner == 0 && mode & 0o022 == 0 && (same || links == 0)
}

/// The entries of [`CODE_LOADING_VARIABLES`] in the systemd user manager's
/// environment, as last read: what the desktop's units start with.
#[cfg(feature = "bus-caller")]
static MANAGER_ENVIRONMENT: std::sync::Mutex<Vec<Vec<u8>>> = std::sync::Mutex::new(Vec::new());

/// The most a process's environment holds: the kernel gives arguments and
/// environment together a quarter of the stack limit, and at most 6 MiB.
#[cfg(feature = "bus-caller")]
const MAX_ENVIRON_BYTES: u64 = 6 << 20;

/// How long the user manager has to say its environment.
#[cfg(feature = "bus-caller")]
const MANAGER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Whether `pid` started with no value of [`CODE_LOADING_VARIABLES`] but one
/// the session hands its programs: this process's own, or the systemd user
/// manager's, which starts the desktop's units with what `environment.d`
/// and the session set (`GTK3_MODULES` for BigStyle, `VK_DRIVER_FILES` for
/// NVIDIA). Read from `/proc/PID/environ`, the environment the program's
/// loader read; one that cannot be read vouches for nothing. The manager is
/// asked again only for a value it was not seen to have.
///
/// This refuses an installed program that a program of the user's started
/// with code of its own to load. Any program of the user's can also set the
/// manager's environment and restart the desktop's units with it, so the
/// manager's values are no weaker than the units themselves. It cannot
/// refuse code already running as the user that hides its traces (rewriting
/// the environment block it started with), nor a parent tracing its child:
/// such code can act as any program of the user's.
#[cfg(feature = "bus-caller")]
pub fn loads_no_foreign_code(pid: u32) -> bool {
    loads_no_foreign_code_beside(pid, &MANAGER_ENVIRONMENT, || {
        zbus::blocking::connection::Builder::session()
            .ok()?
            .method_timeout(MANAGER_TIMEOUT)
            .build()
            .ok()
    })
}

/// [`loads_no_foreign_code`] with `manager`, the user manager's entries as
/// last read, and `bus`, the session bus it is asked on again.
#[cfg(feature = "bus-caller")]
fn loads_no_foreign_code_beside(
    pid: u32,
    manager: &std::sync::Mutex<Vec<Vec<u8>>>,
    bus: impl FnOnce() -> Option<zbus::blocking::Connection>,
) -> bool {
    let environ = format!("/proc/{pid}/environ");
    let Ok(environ) = crate::file_io::read_bounded(Path::new(&environ), MAX_ENVIRON_BYTES) else {
        return false;
    };
    let given = |known: &[Vec<u8>]| {
        environment_loads_no_foreign_code(&environ, |entry| {
            own_entry(entry) || known.iter().any(|kept| kept == entry)
        })
    };
    let known = manager
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if given(&known) {
        return true;
    }
    let Some(fresh) = bus().and_then(|bus| manager_environment(&bus)) else {
        return false;
    };
    let trusted = given(&fresh);
    *manager
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = fresh;
    trusted
}

/// The entries of [`CODE_LOADING_VARIABLES`] in the environment of the
/// systemd user manager on `bus`.
#[cfg(feature = "bus-caller")]
fn manager_environment(bus: &zbus::blocking::Connection) -> Option<Vec<Vec<u8>>> {
    let reply = bus
        .call_method(
            Some("org.freedesktop.systemd1"),
            "/org/freedesktop/systemd1",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.systemd1.Manager", "Environment"),
        )
        .ok()?;
    let value: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
    let entries = Vec::<String>::try_from(value).ok()?;
    Some(
        entries
            .into_iter()
            .map(String::into_bytes)
            .filter(|entry| loads_code(entry))
            .collect(),
    )
}

/// Whether this process has `entry`, `NAME=value`, in its environment.
#[cfg(feature = "bus-caller")]
fn own_entry(entry: &[u8]) -> bool {
    let Some((name, value)) = split_entry(entry) else {
        return false;
    };
    std::str::from_utf8(name)
        .is_ok_and(|name| std::env::var_os(name).is_some_and(|ours| ours.as_bytes() == value))
}

/// `entry`, `NAME=value`, as its name and value.
#[cfg(feature = "bus-caller")]
fn split_entry(entry: &[u8]) -> Option<(&[u8], &[u8])> {
    let separator = entry.iter().position(|&byte| byte == b'=')?;
    let (name, value) = entry.split_at(separator);
    Some((name, value.get(1..).unwrap_or_default()))
}

/// Whether `entry`, `NAME=value`, sets one of [`CODE_LOADING_VARIABLES`].
#[cfg(feature = "bus-caller")]
fn loads_code(entry: &[u8]) -> bool {
    split_entry(entry).is_some_and(|(name, _)| {
        CODE_LOADING_VARIABLES
            .iter()
            .any(|variable| variable.as_bytes() == name)
    })
}

/// Whether every entry of `environ`, NUL-separated `NAME=value` entries,
/// that sets one of [`CODE_LOADING_VARIABLES`] is one the session `given`
/// its programs. Every entry counts: a variable can appear twice, and the
/// loader takes the later one.
#[cfg(feature = "bus-caller")]
fn environment_loads_no_foreign_code(environ: &[u8], given: impl Fn(&[u8]) -> bool) -> bool {
    environ
        .split(|&byte| byte == 0)
        .filter(|entry| loads_code(entry))
        .all(given)
}

/// What `check` finds of `pid`, kept only if `pidfd`, the process that pid
/// named when it was read, has not exited by the end of the check: a pid
/// read after its process ended may name another one, which the check would
/// then vouch for.
pub fn while_alive<T>(
    pidfd: BorrowedFd<'_>,
    pid: u32,
    check: impl FnOnce(u32) -> Option<T>,
) -> Option<T> {
    let found = check(pid)?;
    // A pidfd turns readable once its process has exited; no wait.
    let now = rustix::event::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let mut exited = [rustix::event::PollFd::from_borrowed_fd(
        pidfd,
        rustix::event::PollFlags::IN,
    )];
    let ready = rustix::event::poll(&mut exited, Some(&now));
    matches!(ready, Ok(0)).then_some(found)
}

/// What `check` finds of the process behind the caller's connection in a
/// D-Bus call, by the pidfd the bus holds for it (`ProcessFD`) so that the
/// pid cannot name another process meanwhile. A bus without `ProcessFD`
/// gives the pid alone.
#[cfg(feature = "bus-caller")]
pub async fn check_caller<T>(
    header: &zbus::message::Header<'_>,
    connection: &zbus::Connection,
    check: impl FnOnce(u32) -> Option<T>,
) -> Option<T> {
    check_connection(connection, header.sender()?.clone(), check).await
}

/// What `check` finds of the process behind the bus connection `name`, as
/// [`check_caller`] finds it for the connection that sent a call.
#[cfg(feature = "bus-caller")]
pub async fn check_connection<T>(
    connection: &zbus::Connection,
    name: zbus::names::UniqueName<'_>,
    check: impl FnOnce(u32) -> Option<T>,
) -> Option<T> {
    use std::os::fd::AsFd;
    let dbus = zbus::fdo::DBusProxy::new(connection).await.ok()?;
    let credentials = dbus.get_connection_credentials(name.into()).await.ok()?;
    let pid = credentials.process_id()?;
    match credentials.process_fd() {
        Some(pidfd) => while_alive(pidfd.as_fd(), pid, check),
        None => check(pid),
    }
}

/// Whether polkit lets the bus connection `sender` perform `action`, asking
/// the person for a password when `interactive`. `details` reach polkit's rules
/// and its question (`program`, for one).
///
/// # Errors
///
/// polkit could not be asked or gave no answer: a privileged service refuses
/// the call then.
#[cfg(feature = "bus-caller")]
pub async fn polkit_allows(
    connection: &zbus::Connection,
    sender: &str,
    action: &str,
    interactive: bool,
    details: &[(&str, &str)],
) -> zbus::Result<bool> {
    use std::collections::HashMap;
    let proxy = zbus::Proxy::new(
        connection,
        "org.freedesktop.PolicyKit1",
        "/org/freedesktop/PolicyKit1/Authority",
        "org.freedesktop.PolicyKit1.Authority",
    )
    .await?;
    let subject = (
        "system-bus-name",
        HashMap::from([("name", zbus::zvariant::Value::from(sender))]),
    );
    let details: HashMap<&str, &str> = details.iter().copied().collect();
    // 1 = AllowUserInteraction.
    let flags = u32::from(interactive);
    let (authorized, _challenge, _details): (bool, bool, HashMap<String, String>) = proxy
        .call("CheckAuthorization", &(subject, action, details, flags, ""))
        .await?;
    Ok(authorized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    /// `sleep`, once it runs its own program: a vfork-based spawn returns as
    /// soon as the child starts its exec, while `/proc/PID/exe` may still name
    /// this test's, and the exec names the new program before it has the
    /// environment it reads (`/proc/PID/environ` empty meanwhile).
    fn sleep() -> crate::subprocess::BigSubprocessChild {
        sleep_with(&[])
    }

    /// [`sleep`] with `environment` set beside this process's.
    fn sleep_with(environment: &[(&str, &str)]) -> crate::subprocess::BigSubprocessChild {
        let mut spec = crate::subprocess::BigSubprocessSpec::builder();
        for (name, value) in environment {
            spec = spec.env(*name, *value);
        }
        started(
            &spec.program("/usr/bin/sleep").args(["5"]).build(),
            "/usr/bin/sleep",
        )
    }

    /// The process `spec` starts, once `/proc/PID/exe` names `runs` and its
    /// environment can be read.
    fn started(
        spec: &crate::subprocess::BigSubprocessSpec,
        runs: &str,
    ) -> crate::subprocess::BigSubprocessChild {
        let child = spec.spawn().expect("started");
        let exe = format!("/proc/{}/exe", child.id());
        let environ = format!("/proc/{}/environ", child.id());
        let start = std::time::Instant::now();
        while (std::fs::read_link(&exe).is_ok_and(|running| running != Path::new(runs))
            || std::fs::read(&environ).is_ok_and(|read| read.is_empty()))
            && start.elapsed() < std::time::Duration::from_secs(5)
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        child
    }

    /// The installed file passes, run from its place or replaced since; a
    /// program the user built does not, though it names itself the same.
    #[cfg(feature = "bus-caller")]
    #[test]
    fn caller_runs_installed_file() {
        let mut sleep = sleep();
        let ran = runs_installed(sleep.id(), "/usr/bin/sleep");
        let other = runs_installed(sleep.id(), "/usr/bin/big-body-tracker-daemon");
        let _ = sleep.kill();
        let _ = sleep.wait();
        assert!(ran);
        assert!(!other);
        let built = std::env::current_exe().expect("test program");
        let built = built.to_str().expect("path");
        assert!(!runs_installed(std::process::id(), built));
        let path = "/usr/bin/big-body-tracker-daemon";
        let named = |running: &str| installed_path(Path::new(running)) == Some(path);
        assert!(named(path));
        assert!(named("/usr/bin/big-body-tracker-daemon (deleted)"));
        assert!(!named("/home/me/big-body-tracker-daemon"));
    }

    /// A root-owned program mounted over the installed path, in a namespace
    /// of the user's, is named as the installed one and still refused.
    #[cfg(feature = "bus-caller")]
    #[test]
    fn mounted_over_program_is_refused() {
        let spec = crate::subprocess::BigSubprocessSpec::builder()
            .program("/usr/bin/unshare")
            .args([
                "--user",
                "--map-root-user",
                "--mount",
                "/usr/bin/sh",
                "-c",
                "mount --bind /usr/bin/sleep /usr/bin/true && exec /usr/bin/true 5",
            ])
            .build();
        let mut mounted = started(&spec, "/usr/bin/true");
        let named = std::fs::read_link(format!("/proc/{}/exe", mounted.id()));
        let ran = runs_installed(mounted.id(), "/usr/bin/true");
        let _ = mounted.kill();
        let _ = mounted.wait();
        assert_eq!(
            named.ok().as_deref(),
            Some(Path::new("/usr/bin/true")),
            "the mount took effect"
        );
        assert!(!ran);
    }

    /// Only root's file that nobody else can write passes, and only the file
    /// at the installed path or one an upgrade unlinked.
    #[cfg(feature = "bus-caller")]
    #[test]
    fn installed_file_is_roots_alone() {
        const PROGRAM: u32 = 0o100_755;
        assert!(installed_file(0, PROGRAM, true, 1));
        assert!(
            installed_file(0, PROGRAM, false, 0),
            "unlinked by an upgrade"
        );
        assert!(
            !installed_file(0, PROGRAM, false, 1),
            "another of root's files"
        );
        assert!(
            !installed_file(0, 0o100_775, true, 1),
            "writable by the group"
        );
        assert!(!installed_file(0, 0o100_757, true, 1), "writable by all");
        assert!(!installed_file(1000, PROGRAM, true, 1), "the user's");
    }

    /// Another user's process, whose environment this one cannot read,
    /// vouches for nothing.
    #[cfg(feature = "bus-caller")]
    #[test]
    fn unreadable_environment_vouches_for_nothing() {
        assert!(
            std::fs::read("/proc/1/environ").is_err(),
            "pid 1 belongs to another user"
        );
        let known = std::sync::Mutex::new(Vec::new());
        assert!(!loads_no_foreign_code_beside(1, &known, || None));
    }

    /// A process checked while alive passes; one that exited is refused,
    /// as its pid may name another process by then.
    #[test]
    fn exited_process_is_refused() {
        let mut sleep = sleep();
        let pid = sleep.id();
        let pidfd = crate::pidfd::open(pid).expect("pidfd_open");
        assert_eq!(while_alive(pidfd.as_fd(), pid, Some), Some(pid));
        let _ = sleep.kill();
        let _ = sleep.wait();
        assert_eq!(while_alive(pidfd.as_fd(), pid, Some), None);
    }

    /// The installed file started with a module to load vouches for nothing.
    #[cfg(feature = "bus-caller")]
    #[test]
    fn preloaded_installed_file_is_refused() {
        let mut sleep = sleep_with(&[("GTK_MODULES", "/home/someone/evil.so")]);
        let ran = runs_installed(sleep.id(), "/usr/bin/sleep");
        let _ = sleep.kill();
        let _ = sleep.wait();
        assert!(!ran);
    }

    /// A variable that loads code passes only with a value the session gave;
    /// every other variable, and an entry that names none, passes.
    #[cfg(feature = "bus-caller")]
    #[test]
    fn foreign_code_environment_is_refused() {
        let given = |entry: &[u8]| entry == b"LD_LIBRARY_PATH=/opt/lib";
        let clean = |environ: &[u8]| environment_loads_no_foreign_code(environ, given);
        assert!(clean(b""));
        assert!(clean(b"HOME=/home/p\0PATH=/usr/bin\0"));
        assert!(
            clean(b"LD_LIBRARY_PATH=/opt/lib\0HOME=/home/p"),
            "the session's own"
        );
        assert!(clean(b"LD_PRELOAD_NOT=x\0XLD_PRELOAD=x\0LD_PRELOAD\0"));
        assert!(!clean(b"HOME=/home/p\0LD_PRELOAD=/home/p/evil.so\0"));
        assert!(!clean(b"GIO_EXTRA_MODULES=/home/p/gio"));
        assert!(!clean(b"GCONV_PATH=/home/p/gconv"), "iconv modules");
        assert!(!clean(b"__EGL_EXTERNAL_PLATFORM_CONFIG_DIRS=/home/p/egl"));
        assert!(!clean(b"LD_PRELOAD="), "set, though empty");
        assert!(!clean(b"LD_LIBRARY_PATH=/home/p/lib"), "another value");
        assert!(
            !clean(b"LD_LIBRARY_PATH=/opt/lib\0LD_LIBRARY_PATH=/home/p/lib"),
            "the loader takes the later one"
        );
    }

    /// A stand-in for the systemd user manager, saying its environment.
    #[cfg(all(feature = "bus-caller", feature = "test-bus"))]
    struct Manager(Vec<String>);

    #[cfg(all(feature = "bus-caller", feature = "test-bus"))]
    #[zbus::interface(name = "org.freedesktop.systemd1.Manager")]
    impl Manager {
        #[zbus(property)]
        fn environment(&self) -> Vec<String> {
            self.0.clone()
        }
    }

    /// A program started as the desktop's units are, with values of the user
    /// manager's that this process lacks (BigStyle's GTK 3 module, NVIDIA's
    /// Vulkan driver), vouches for itself; the same program with a value of
    /// its own does not, nor with no manager to ask.
    #[cfg(all(feature = "bus-caller", feature = "test-bus"))]
    #[test]
    fn session_environment_is_trusted() {
        const MODULES: (&str, &str) = ("GTK3_MODULES", "big-session-test-gtk3");
        const DRIVER: (&str, &str) = (
            "VK_DRIVER_FILES",
            "/usr/share/vulkan/icd.d/big_session_test_icd.json",
        );
        for (name, value) in [MODULES, DRIVER] {
            assert_ne!(
                std::env::var_os(name).as_deref(),
                Some(std::ffi::OsStr::new(value)),
                "the checker lacks {name}"
            );
        }
        let dir = std::env::temp_dir().join(format!("caller-session-{}", std::process::id()));
        let (mut daemon, address) =
            crate::test_bus::activating(&dir, "org.example.Unused").expect("dbus-daemon");
        let connect = || {
            zbus::blocking::connection::Builder::address(address.as_str())
                .ok()?
                .build()
                .ok()
        };
        let _manager = zbus::blocking::connection::Builder::address(address.as_str())
            .expect("an address")
            .name("org.freedesktop.systemd1")
            .expect("a name")
            .serve_at(
                "/org/freedesktop/systemd1",
                Manager(vec![
                    "HOME=/home/someone".to_owned(),
                    format!("{}={}", MODULES.0, MODULES.1),
                    format!("{}={}", DRIVER.0, DRIVER.1),
                ]),
            )
            .expect("a path")
            .build()
            .expect("the stand-in manager connects");
        let check =
            |environment: &[(&str, &str)], known: &std::sync::Mutex<Vec<Vec<u8>>>, ask| {
                let mut sleep = sleep_with(environment);
                let trusted = loads_no_foreign_code_beside(sleep.id(), known, || {
                    if ask { connect() } else { None }
                });
                let _ = sleep.kill();
                let _ = sleep.wait();
                trusted
            };
        let known = std::sync::Mutex::new(Vec::new());
        let session = check(&[MODULES, DRIVER], &known, true);
        let remembered = check(&[MODULES], &known, false);
        let preloaded = check(
            &[MODULES, DRIVER, ("LD_PRELOAD", "/nonexistent/evil.so")],
            &known,
            true,
        );
        let other_module = check(&[("GTK3_MODULES", "evil")], &known, true);
        let no_manager = check(&[MODULES], &std::sync::Mutex::new(Vec::new()), false);
        let _ = daemon.kill();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(session, "the user manager's values");
        assert!(remembered, "known without asking again");
        assert!(!preloaded, "a value of its own beside the session's");
        assert!(!other_module, "another value");
        assert!(!no_manager, "nothing to vouch for the value");
    }

    /// A bus where polkit does not answer gives an error, never a grant: the
    /// services refuse the call on it.
    #[cfg(all(feature = "bus-caller", feature = "test-bus"))]
    #[test]
    fn unanswered_polkit_grants_nothing() {
        let dir = std::env::temp_dir().join(format!("caller-polkit-gone-{}", std::process::id()));
        let (mut daemon, address) =
            crate::test_bus::activating(&dir, "org.freedesktop.PolicyKit1").expect("dbus-daemon");
        let answer = zbus::block_on(async {
            let connection = zbus::connection::Builder::address(address.as_str())?
                .build()
                .await?;
            polkit_allows(&connection, ":1.1", "org.example.action", false, &[]).await
        });
        let _ = daemon.kill();
        let _ = daemon.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(answer.is_err(), "{answer:?}");
    }

    /// A stand-in polkit authority: grants `org.example.allowed` alone, and
    /// keeps each question it was asked.
    #[cfg(all(feature = "bus-caller", feature = "test-bus"))]
    struct Authority(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    #[cfg(all(feature = "bus-caller", feature = "test-bus"))]
    #[zbus::interface(name = "org.freedesktop.PolicyKit1.Authority")]
    impl Authority {
        fn check_authorization(
            &self,
            subject: (
                String,
                std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
            ),
            action: String,
            details: std::collections::BTreeMap<String, String>,
            flags: u32,
            _cancellation: String,
        ) -> (bool, bool, std::collections::HashMap<String, String>) {
            let name = subject
                .1
                .get("name")
                .and_then(|value| <&str>::try_from(value).ok())
                .unwrap_or_default()
                .to_owned();
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(format!("{} {name} {action} {details:?} {flags}", subject.0));
            (
                action == "org.example.allowed",
                false,
                std::collections::HashMap::new(),
            )
        }
    }

    /// polkit's own answer decides, and the question carries the caller as a
    /// bus name, the action, the details and the interaction flag unchanged.
    #[cfg(all(feature = "bus-caller", feature = "test-bus"))]
    #[test]
    fn polkit_answer_decides() {
        let dir = std::env::temp_dir().join(format!("caller-polkit-{}", std::process::id()));
        let (mut daemon, address) =
            crate::test_bus::activating(&dir, "org.example.Unused").expect("dbus-daemon");
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let _authority = zbus::blocking::connection::Builder::address(address.as_str())
            .expect("an address")
            .name("org.freedesktop.PolicyKit1")
            .expect("a name")
            .serve_at(
                "/org/freedesktop/PolicyKit1/Authority",
                Authority(std::sync::Arc::clone(&asked)),
            )
            .expect("served")
            .build()
            .expect("the stand-in authority");
        let answers = zbus::block_on(async {
            let connection = zbus::connection::Builder::address(address.as_str())?
                .build()
                .await?;
            let denied = polkit_allows(
                &connection,
                ":1.7",
                "org.example.denied",
                true,
                &[("program", "x")],
            )
            .await?;
            let allowed =
                polkit_allows(&connection, ":1.7", "org.example.allowed", false, &[]).await?;
            zbus::Result::Ok((denied, allowed))
        });
        let _ = daemon.kill();
        let _ = daemon.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(answers.expect("polkit answered"), (false, true));
        let asked = asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert_eq!(
            asked,
            [
                r#"system-bus-name :1.7 org.example.denied {"program": "x"} 1"#,
                "system-bus-name :1.7 org.example.allowed {} 0",
            ]
        );
    }
}
