//! Landlock rules for a loader that runs without bubblewrap: the parent builds
//! the ruleset, the forked child enforces it before it executes the loader.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;

use crate::Error;

// linux/landlock.h
const CREATE_RULESET_VERSION: u32 = 1;
const RULE_PATH_BENEATH: libc::c_int = 1;
pub const EXECUTE: u64 = 1 << 0;
pub const WRITE_FILE: u64 = 1 << 1;
pub const READ_FILE: u64 = 1 << 2;
pub const READ_DIR: u64 = 1 << 3;
const REMOVE_DIR: u64 = 1 << 4;
const REMOVE_FILE: u64 = 1 << 5;
const MAKE_DIR: u64 = 1 << 7;
const MAKE_REG: u64 = 1 << 8;
/// Every right ABI 1 knows: creating, removing and reading files, directories,
/// sockets, pipes, devices and links.
const ABI_1: u64 = (1 << 13) - 1;
const REFER: u64 = 1 << 13;
const TRUNCATE: u64 = 1 << 14;
const IOCTL_DEV: u64 = 1 << 15;
/// The rights a rule on a file, not a directory, may grant.
const FILE_RIGHTS: u64 = EXECUTE | WRITE_FILE | READ_FILE | TRUNCATE | IOCTL_DEV;
const SCOPE_SIGNAL: u64 = 1 << 1;

pub const READ: u64 = READ_FILE | READ_DIR;
/// What fontconfig does in its cache: write new files and replace old ones.
pub const CHANGE: u64 = WRITE_FILE | REMOVE_DIR | REMOVE_FILE | MAKE_DIR | MAKE_REG | TRUNCATE;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

pub struct LandlockRuleset(OwnedFd);

impl LandlockRuleset {
    /// A ruleset that denies every file access the kernel knows except `rules`,
    /// and, from ABI 6, signals to processes outside it. A path that cannot be
    /// opened grants nothing.
    ///
    /// [`Error::SandboxUnavailable`] when the kernel has no Landlock.
    pub fn new(rules: &[(PathBuf, u64)]) -> Result<Self, Error> {
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
            return Err(Error::SandboxUnavailable);
        }
        let mut handled = ABI_1;
        if abi >= 2 {
            handled |= REFER;
        }
        if abi >= 3 {
            handled |= TRUNCATE;
        }
        if abi >= 5 {
            handled |= IOCTL_DEV;
        }
        let attr = RulesetAttr {
            handled_access_fs: handled,
            handled_access_net: 0,
            scoped: if abi >= 6 { SCOPE_SIGNAL } else { 0 },
        };
        // SAFETY: `attr` is a ruleset attribute of the size given; a kernel that
        // knows fewer fields accepts the ones after them as zero.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &raw const attr,
                size_of::<RulesetAttr>(),
                0,
            )
        };
        let fd = i32::try_from(fd).map_err(std::io::Error::other)?;
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: a descriptor the call just returned, owned by nobody else.
        let ruleset = unsafe { OwnedFd::from_raw_fd(fd) };
        for (path, access) in rules {
            add_path_rule(&ruleset, path, access & handled)?;
        }
        Ok(Self(ruleset))
    }

    /// Bind the calling thread, and what it executes, to the ruleset. Plain
    /// system calls only, for a forked child.
    pub fn restrict_self(&self) -> std::io::Result<()> {
        // SAFETY: prctl and a system call on a valid descriptor.
        let restricted = unsafe {
            libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0
                && libc::syscall(libc::SYS_landlock_restrict_self, self.0.as_raw_fd(), 0) == 0
        };
        if restricted {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

/// Grant `access` beneath `path` in `ruleset`, narrowed to what a file may take
/// when `path` is no directory; a path that cannot be opened grants nothing.
fn add_path_rule(ruleset: &OwnedFd, path: &PathBuf, access: u64) -> Result<(), Error> {
    let Ok(file) = nix::fcntl::open(
        path.as_path(),
        nix::fcntl::OFlag::O_PATH | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    ) else {
        return Ok(());
    };
    let mut access = access;
    if !nix::sys::stat::fstat(&file).is_ok_and(|stat| stat.st_mode & libc::S_IFMT == libc::S_IFDIR)
    {
        access &= FILE_RIGHTS;
    }
    let beneath = PathBeneathAttr {
        allowed_access: access,
        parent_fd: file.as_raw_fd(),
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
        return Err(Error::StdIoError {
            err: std::sync::Arc::new(std::io::Error::last_os_error()),
            info: format!("Landlock refused {}", path.display()),
        });
    }
    Ok(())
}
