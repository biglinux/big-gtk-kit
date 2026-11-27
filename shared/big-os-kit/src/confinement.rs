// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Confinement for programs that read untrusted input: a Bubblewrap
//! namespace for a command started from here, or Landlock rules a process
//! that draws with the GPU sets on itself.

use std::ffi::{CString, OsString};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

/// The Bubblewrap the sandboxes run.
pub const BWRAP: &str = "/usr/bin/bwrap";

/// The start of a bwrap command line: a namespace holding the system's
/// programs and libraries read-only, with fresh `/dev`, `/proc` and `/tmp`.
///
/// Nothing of the person's is in it: no home, no `/run` (so no session bus),
/// no network, no other process. The caller binds the paths its command
/// needs, then adds `--` and the command.
#[must_use]
pub fn system_only_namespace() -> Vec<OsString> {
    [
        "--ro-bind",
        "/usr",
        "/usr",
        "--symlink",
        "usr/lib",
        "/lib",
        "--symlink",
        "usr/lib",
        "/lib64",
        "--symlink",
        "usr/bin",
        "/bin",
        "--symlink",
        "usr/bin",
        "/sbin",
        "--ro-bind-try",
        "/etc/ld.so.cache",
        "/etc/ld.so.cache",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--tmpfs",
        "/tmp",
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
    ]
    .map(OsString::from)
    .into()
}

// linux/landlock.h, ABI 1 to 3.
const CREATE_RULESET_VERSION: u32 = 1;
const RULE_PATH_BENEATH: libc::c_int = 1;
const EXECUTE: u64 = 1 << 0;
const WRITE_FILE: u64 = 1 << 1;
const READ_FILE: u64 = 1 << 2;
const READ_DIR: u64 = 1 << 3;
const REMOVE_DIR: u64 = 1 << 4;
const REMOVE_FILE: u64 = 1 << 5;
const MAKE_CHAR: u64 = 1 << 6;
const MAKE_DIR: u64 = 1 << 7;
const MAKE_REG: u64 = 1 << 8;
const MAKE_SOCK: u64 = 1 << 9;
const MAKE_FIFO: u64 = 1 << 10;
const MAKE_BLOCK: u64 = 1 << 11;
const MAKE_SYM: u64 = 1 << 12;
const REFER: u64 = 1 << 13;
const TRUNCATE: u64 = 1 << 14;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

const READ: u64 = READ_FILE | READ_DIR;
const CHANGE: u64 = WRITE_FILE | REMOVE_DIR | REMOVE_FILE | MAKE_DIR | MAKE_REG | MAKE_SYM;

/// Confine this process with Landlock to what a GPU program needs, for one
/// that is about to read a shader somebody else wrote.
///
/// Read: the folders of `readable`, and `/usr`, `/etc`, `/proc` and `/sys`,
/// where the GPU driver finds its libraries and settings. Read and write:
/// `/dev`, the driver's device nodes, and the drivers' shader caches under
/// `$XDG_CACHE_HOME`. Nothing else of the person's home: a shader's
/// `#include "../../.ssh/id_ed25519"` fails to open. Descriptors already open
/// (a Wayland socket, a pipe) keep working. Landlock binds this thread and the
/// ones it starts later: a thread the driver started before this, a shader
/// compiler's among them, reads with no limit. Call it after EGL and the GL
/// context are open. A kernel without Landlock leaves the process unconfined,
/// which is logged; one before Linux 6.2 (Landlock ABI 1 or 2) cannot deny
/// truncating a file, so that stays allowed wherever this user may write.
///
/// # Errors
///
/// When the kernel has Landlock and a rule is not taken.
pub fn confine_gpu_process<'a>(
    readable: impl IntoIterator<Item = &'a Path>,
) -> std::io::Result<()> {
    // SAFETY: the version query takes no attributes and returns a number.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0,
            CREATE_RULESET_VERSION,
        )
    };
    if abi < 1 {
        log::warn!(
            "no Landlock here ({}): shaders may read any file this user can",
            std::io::Error::last_os_error()
        );
        return Ok(());
    }
    let mut handled = EXECUTE | READ | CHANGE | MAKE_CHAR | MAKE_SOCK | MAKE_FIFO | MAKE_BLOCK;
    if abi >= 2 {
        handled |= REFER;
    }
    if abi >= 3 {
        handled |= TRUNCATE;
    }
    let attr = RulesetAttr {
        handled_access_fs: handled,
    };
    // SAFETY: `attr` is a ruleset attribute of the size given.
    let ruleset = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &raw const attr,
            size_of::<RulesetAttr>(),
            0,
        )
    };
    if ruleset < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let ruleset = i32::try_from(ruleset).map_err(std::io::Error::other)?;
    // SAFETY: a descriptor the call just returned, owned by nobody else.
    let ruleset = unsafe { OwnedFd::from_raw_fd(ruleset) };
    let caches = crate::settings::cache_home_checked();
    let mut rules: Vec<(PathBuf, u64)> = readable
        .into_iter()
        .map(|folder| (folder.to_owned(), READ))
        .collect();
    rules.extend([
        ("/usr".into(), READ),
        ("/etc".into(), READ),
        ("/proc".into(), READ),
        ("/sys".into(), READ),
        ("/dev".into(), READ | WRITE_FILE),
    ]);
    if let Some(caches) = caches {
        for cache in ["mesa_shader_cache", "mesa_shader_cache_db", "nvidia"] {
            rules.push((caches.join(cache), READ | CHANGE | TRUNCATE));
        }
    }
    for (path, access) in rules {
        let Ok(name) = CString::new(path.as_os_str().as_bytes()) else {
            continue;
        };
        // SAFETY: a NUL-terminated path; the descriptor, when one comes, is
        // owned by the `OwnedFd` below.
        let fd = unsafe { libc::open(name.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if fd < 0 {
            // A cache that does not exist yet is not made here.
            continue;
        }
        // SAFETY: just opened, owned by nobody else.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let beneath = PathBeneathAttr {
            allowed_access: access & handled,
            parent_fd: fd.as_raw_fd(),
        };
        // SAFETY: the ruleset and the attribute are valid for the call.
        let added = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset.as_raw_fd(),
                RULE_PATH_BENEATH,
                &raw const beneath,
                0,
            )
        };
        if added < 0 {
            let error = std::io::Error::last_os_error();
            return Err(std::io::Error::new(
                error.kind(),
                format!("Landlock refused {}: {error}", path.display()),
            ));
        }
    }
    // SAFETY: plain prctl and syscall on this thread; the ruleset is valid.
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) < 0
            || libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0) < 0
        {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subprocess::BigSubprocessSpec;

    /// What a confined command can see: the system's programs, and neither
    /// the home folder, the session bus nor a file beside the one bound in.
    #[test]
    fn namespace_hides_home_and_bus() {
        let runtime = format!("/run/user/{}", crate::users::own_uid());
        assert!(Path::new(&runtime).is_dir());
        let folder = std::env::temp_dir().join(format!("big-confinement-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("bound"), b"in").unwrap();
        std::fs::write(folder.join("beside"), b"out").unwrap();
        let bound = folder.join("bound");
        let probe = |path: &OsString| {
            BigSubprocessSpec::builder()
                .program(BWRAP)
                .args(system_only_namespace())
                .arg("--ro-bind")
                .arg(&bound)
                .arg(&bound)
                .args(["--", "/usr/bin/test", "-e"])
                .arg(path)
                .allow_list([BWRAP])
                .build()
                .run()
                .unwrap()
                .status
                .success()
        };
        assert!(probe(&bound.clone().into_os_string()), "the bound file");
        assert!(!probe(&folder.join("beside").into_os_string()));
        // The folder that holds the session bus socket.
        assert!(!probe(&runtime.into()));
        // Something of the home folder that the bound file does not lie in.
        let home = std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME"));
        let elsewhere = std::fs::read_dir(&home)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| !bound.starts_with(path))
            .expect("an entry of the home folder");
        assert!(!probe(&elsewhere.into_os_string()));
        std::fs::remove_dir_all(folder).unwrap();
    }

    /// The output of `command` run in the namespace.
    fn confined_output(command: &[&str]) -> String {
        let output = BigSubprocessSpec::builder()
            .program(BWRAP)
            .args(system_only_namespace())
            .arg("--")
            .args(command)
            .allow_list([BWRAP])
            .build()
            .run()
            .unwrap();
        assert!(output.status.success(), "{}", output.stderr_lossy());
        output.stdout_lossy()
    }

    /// A confined command sees no other process than bwrap's own and no
    /// network but its own loopback.
    #[test]
    fn namespace_hides_processes_and_network() {
        let listing = confined_output(&["/usr/bin/ls", "/proc"]);
        let processes: Vec<&str> = listing
            .lines()
            .filter(|name| name.bytes().all(|byte| byte.is_ascii_digit()))
            .collect();
        assert_eq!(processes, ["1", "2"], "bwrap and the command");
        let devices = confined_output(&["/usr/bin/cat", "/proc/net/dev"]);
        let interfaces: Vec<&str> = devices
            .lines()
            .skip(2)
            .filter_map(|line| line.split_once(':'))
            .map(|(name, _)| name.trim())
            .collect();
        assert_eq!(interfaces, ["lo"]);
    }

    /// The confined command ends with the thread that started bwrap, which
    /// the kernel's death signal counts as its parent, though it would run on.
    #[test]
    fn namespace_ends_with_its_parent() {
        let mut confined = std::thread::spawn(|| {
            let mut confined = BigSubprocessSpec::builder()
                .program(BWRAP)
                .args(system_only_namespace())
                .args(["--", "/usr/bin/sh", "-c", "echo started && exec sleep 30"])
                .allow_list([BWRAP])
                .build()
                .spawn()
                .unwrap();
            let mut started = String::new();
            std::io::BufRead::read_line(
                &mut std::io::BufReader::new(confined.take_stdout().unwrap()),
                &mut started,
            )
            .unwrap();
            assert_eq!(started, "started\n");
            confined
        })
        .join()
        .unwrap();
        let start = std::time::Instant::now();
        let mut ended = confined.try_wait().unwrap();
        while ended.is_none() && start.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(10));
            ended = confined.try_wait().unwrap();
        }
        if ended.is_none() {
            let _ = confined.kill();
            let _ = confined.wait();
        }
        assert!(ended.is_some_and(|status| !status.success()), "{ended:?}");
    }

    /// Each folder given reads, and nothing beside them does, nor takes a
    /// write.
    #[test]
    fn confined_thread_reads_only_given_folders() {
        let root = std::env::temp_dir().join(format!("landlock-{}", std::process::id()));
        for (folder, file) in [("shaders", "grey.slang"), ("own", "crt.glsl")] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
            std::fs::write(root.join(folder).join(file), "shader").unwrap();
        }
        std::fs::write(root.join("secret"), "secret").unwrap();
        let confined = std::thread::spawn({
            let root = root.clone();
            move || {
                let folders = [root.join("shaders"), root.join("own")];
                confine_gpu_process(folders.iter().map(PathBuf::as_path)).unwrap();
                (
                    std::fs::read_to_string(root.join("shaders/grey.slang")).ok(),
                    std::fs::read_to_string(root.join("own/crt.glsl")).ok(),
                    std::fs::read_to_string(root.join("secret")).ok(),
                    std::fs::write(root.join("own/new"), "x").is_ok(),
                )
            }
        })
        .join()
        .unwrap();
        let shader = Some("shader".to_owned());
        assert_eq!(confined, (shader.clone(), shader, None, false));
        std::fs::remove_dir_all(root).unwrap();
    }
}
