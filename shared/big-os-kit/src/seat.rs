// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Whether a desktop is the computer's: only a desktop whose compositor runs
//! in a logind session on a seat may act on the machine's power, as locks,
//! sleep, shutdown or the GPU. A hidden test session started from a terminal
//! or over ssh has a compositor in no session or a seatless one.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use zbus::zvariant::OwnedObjectPath;

/// The logind session `compositor` (a process id) runs in, when it is on a
/// seat.
///
/// # Errors
///
/// Returns logind's error when the process is in no session, and a failure
/// when its session has no seat or `compositor` is 0, which logind reads as
/// the caller.
pub async fn seated_session(
    system: &zbus::Connection,
    compositor: u32,
) -> zbus::Result<OwnedObjectPath> {
    if compositor == 0 {
        return Err(zbus::Error::Failure(
            "no compositor process to ask logind about".to_owned(),
        ));
    }
    let session: OwnedObjectPath = system
        .call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "GetSessionByPID",
            &(compositor,),
        )
        .await?
        .body()
        .deserialize()?;
    let proxy = zbus::Proxy::new(
        system,
        "org.freedesktop.login1",
        session.as_str(),
        "org.freedesktop.login1.Session",
    )
    .await?;
    let (seat, _): (String, OwnedObjectPath) = proxy.get_property("Seat").await?;
    if seat.is_empty() {
        return Err(zbus::Error::Failure(format!(
            "the compositor's session {} has no seat",
            session.as_str()
        )));
    }
    Ok(session)
}

/// The Wayland socket `WAYLAND_DISPLAY` names, under `XDG_RUNTIME_DIR` unless
/// it is absolute. Unset is no display: the default `wayland-0` may be
/// another session's.
///
/// # Errors
///
/// Returns `NotFound` when either variable needed is unset.
pub fn wayland_socket() -> io::Result<PathBuf> {
    let unset = |name| io::Error::new(io::ErrorKind::NotFound, format!("{name} is not set"));
    let display =
        PathBuf::from(std::env::var_os("WAYLAND_DISPLAY").ok_or_else(|| unset("WAYLAND_DISPLAY"))?);
    if display.is_absolute() {
        return Ok(display);
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| unset("XDG_RUNTIME_DIR"))?;
    Ok(PathBuf::from(runtime).join(display))
}

/// The process listening on the Wayland socket `socket`, for a program that
/// is no Wayland client itself.
///
/// # Errors
///
/// Returns the OS error when the socket does not answer, and `NotFound` when
/// the compositor runs outside this process's PID namespace.
pub fn compositor_pid(socket: &std::path::Path) -> io::Result<u32> {
    let stream = UnixStream::connect(socket)?;
    let pid = crate::microdaemon::peer_process_id(std::os::fd::AsFd::as_fd(&stream))?;
    if pid == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the compositor runs outside this PID namespace",
        ));
    }
    u32::try_from(pid).map_err(io::Error::other)
}

#[cfg(all(test, feature = "test-bus"))]
mod tests {
    use super::*;
    use zbus::zvariant::ObjectPath;

    const SESSION: &str = "/org/freedesktop/login1/session/c1";

    /// A stand-in logind, which answers for every process, pid 0 included,
    /// as logind answers for the caller: with a session on a seat.
    struct Manager;

    #[zbus::interface(name = "org.freedesktop.login1.Manager")]
    impl Manager {
        #[zbus(name = "GetSessionByPID")]
        fn get_session_by_pid(&self, _pid: u32) -> OwnedObjectPath {
            ObjectPath::from_static_str_unchecked(SESSION).into()
        }
    }

    struct Session;

    #[zbus::interface(name = "org.freedesktop.login1.Session")]
    impl Session {
        #[zbus(property)]
        fn seat(&self) -> (String, OwnedObjectPath) {
            (
                "seat0".to_owned(),
                ObjectPath::from_static_str_unchecked("/org/freedesktop/login1/seat/seat0").into(),
            )
        }
    }

    /// A compositor outside this PID namespace reads as pid 0, which names
    /// no session whatever logind answers; a seated compositor's pid does.
    #[test]
    fn hidden_compositor_has_no_session() {
        let dir = std::env::temp_dir().join(format!("seat-logind-{}", std::process::id()));
        let (mut daemon, address) =
            crate::test_bus::activating(&dir, "org.example.Unused").expect("dbus-daemon");
        let _logind = zbus::blocking::connection::Builder::address(address.as_str())
            .expect("an address")
            .name("org.freedesktop.login1")
            .expect("a name")
            .serve_at("/org/freedesktop/login1", Manager)
            .expect("the manager")
            .serve_at(SESSION, Session)
            .expect("the session")
            .build()
            .expect("the stand-in logind");
        let answers = zbus::block_on(async {
            let system = zbus::connection::Builder::address(address.as_str())?
                .build()
                .await?;
            let hidden = seated_session(&system, 0).await;
            let seated = seated_session(&system, 4242).await?;
            zbus::Result::Ok((hidden, seated))
        });
        let _ = daemon.kill();
        let _ = daemon.wait();
        let _ = std::fs::remove_dir_all(&dir);
        let (hidden, seated) = answers.expect("logind answered");
        assert!(hidden.is_err(), "{hidden:?}");
        assert_eq!(seated.as_str(), SESSION);
    }
}
