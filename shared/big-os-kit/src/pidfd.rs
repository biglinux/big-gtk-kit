// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Process descriptors: a pidfd names one process for as long as it is held,
//! so a signal sent through it never reaches another process that later gets
//! the same number.

use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};

use rustix::process::{Pid, PidfdFlags, Signal};

/// A descriptor for the process `pid`, readable once that process exits.
///
/// It names whichever process has the number now: open it for a child not yet
/// reaped, or check the process after opening it.
///
/// # Errors
///
/// `pid` is 0 or does not fit a `pid_t`, no process has it, or the kernel has
/// no `pidfd_open` (before Linux 5.3).
pub fn open(pid: u32) -> io::Result<OwnedFd> {
    let pid = i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
    Ok(rustix::process::pidfd_open(pid, PidfdFlags::empty())?)
}

/// Send `signal` to the process `pidfd` names.
///
/// # Errors
///
/// `signal` is no named signal, the process has exited (`ESRCH`), the caller
/// may not signal it, or the kernel has no `pidfd_send_signal` (before Linux
/// 5.1).
pub fn send_signal(pidfd: BorrowedFd<'_>, signal: libc::c_int) -> io::Result<()> {
    let signal =
        Signal::from_named_raw(signal).ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
    Ok(rustix::process::pidfd_send_signal(pidfd, signal)?)
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;

    /// A child is killed through its pidfd: it ends with no exit code, and a
    /// second signal to the reaped process fails. A number no `pid_t` holds is
    /// refused.
    #[test]
    fn signal_reaches_child() {
        let mut child = crate::subprocess::BigSubprocessSpec::builder()
            .program("/usr/bin/sleep")
            .args(["30"])
            .build()
            .spawn()
            .expect("sleep");
        let pidfd = super::open(child.id()).expect("pidfd");
        super::send_signal(pidfd.as_fd(), libc::SIGKILL).expect("signal");
        let status = child.wait().expect("wait");
        assert!(!status.success());
        assert_eq!(status.code(), None);
        assert!(super::send_signal(pidfd.as_fd(), libc::SIGKILL).is_err());
        assert!(super::open(u32::MAX).is_err());
        assert!(super::open(0).is_err());
    }
}
