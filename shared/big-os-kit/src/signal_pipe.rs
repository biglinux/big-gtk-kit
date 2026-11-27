// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Signals delivered as bytes on a pipe, so a loop or a thread waits for them
//! beside its other descriptors.
//!
//! Handlers rather than a blocked mask and a signalfd: a blocked mask outlives
//! exec, and every program the process starts would run with those signals
//! blocked. Exec resets handlers.

use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, Ordering};

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::pipe::{PipeFlags, pipe_with};
use rustix::process::Signal;

/// The pipe's write end, open for the life of the process once installed.
static WRITE_END: OnceLock<OwnedFd> = OnceLock::new();

/// [`WRITE_END`]'s descriptor, the one value the handler reads.
static WRITE_FD: AtomicI32 = AtomicI32::new(-1);

/// Have each of `signals` write its number, one byte, to a pipe, and return the
/// pipe's read end (blocking, close-on-exec).
///
/// One pipe serves the whole process. A full pipe drops the byte, which a
/// reader that drains what it finds does not miss.
///
/// # Errors
///
/// A second call (`AlreadyExists`), the pipe's creation, or a signal whose
/// action cannot change (`SIGKILL`, `SIGSTOP`).
pub fn install(signals: &[Signal]) -> io::Result<OwnedFd> {
    let (read, write) = pipe_with(PipeFlags::CLOEXEC)?;
    fcntl_setfl(&write, fcntl_getfl(&write)? | OFlags::NONBLOCK)?;
    let raw = write.as_raw_fd();
    if WRITE_END.set(write).is_err() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "signals already go to a pipe",
        ));
    }
    WRITE_FD.store(raw, Ordering::Release);
    for signal in signals {
        #[expect(clippy::disallowed_methods, reason = "the owner of the ban")]
        // SAFETY: a zeroed sigaction is an empty one, and the handler makes
        // only async-signal-safe calls.
        let installed = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = deliver as extern "C" fn(libc::c_int) as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&raw mut action.sa_mask);
            libc::sigaction(signal.as_raw(), &raw const action, std::ptr::null_mut())
        };
        if installed != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(read)
}

extern "C" fn deliver(signal: libc::c_int) {
    // Signal numbers end at 64.
    let byte = u8::try_from(signal).unwrap_or(u8::MAX);
    // SAFETY: write(2) is async-signal-safe. errno belongs to the interrupted
    // code, and a full pipe sets it (EAGAIN), so it is put back.
    unsafe {
        let errno = libc::__errno_location();
        let saved = *errno;
        libc::write(
            WRITE_FD.load(Ordering::Acquire),
            (&raw const byte).cast(),
            1,
        );
        *errno = saved;
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::{AsFd, BorrowedFd};

    use super::*;

    /// Raise `signal` on the calling thread, whose errno the handler shares.
    fn raise_here(signal: Signal) {
        // SAFETY: the calling thread's own id, alive for the call.
        let sent = unsafe { libc::pthread_kill(libc::pthread_self(), signal.as_raw()) };
        assert_eq!(sent, 0, "raised");
    }

    /// One test, since a process installs once: the byte names the signal, a
    /// full pipe leaves the interrupted code's errno alone, and a second
    /// install is refused.
    #[test]
    fn signal_arrives_as_its_number() {
        let read = install(&[Signal::USR2]).expect("installed");
        raise_here(Signal::USR2);
        let mut byte = [0u8; 1];
        rustix::io::read(&read, &mut byte).expect("read");
        assert_eq!(i32::from(byte[0]), Signal::USR2.as_raw());

        let write = WRITE_END.get().expect("write end").as_fd();
        fill(&read, write);
        // SAFETY: this thread's errno slot, valid for the thread's life.
        unsafe { *libc::__errno_location() = libc::ERANGE };
        raise_here(Signal::USR2);
        // SAFETY: as above.
        assert_eq!(unsafe { *libc::__errno_location() }, libc::ERANGE);

        let again = install(&[Signal::USR2]).expect_err("second install");
        assert_eq!(again.kind(), io::ErrorKind::AlreadyExists);
    }

    fn fill(read: &OwnedFd, write: BorrowedFd<'_>) {
        let size = rustix::pipe::fcntl_getpipe_size(read).expect("pipe size");
        let filler = vec![0u8; size];
        let written = rustix::io::write(write, &filler).expect("filled");
        assert_eq!(written, size);
    }
}
