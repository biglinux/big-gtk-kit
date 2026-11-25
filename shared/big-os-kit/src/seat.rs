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
/// when its session has no seat.
pub async fn seated_session(
    system: &zbus::Connection,
    compositor: u32,
) -> zbus::Result<OwnedObjectPath> {
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
/// Returns the OS error when the socket does not answer.
pub fn compositor_pid(socket: &std::path::Path) -> io::Result<u32> {
    let stream = UnixStream::connect(socket)?;
    let pid = crate::microdaemon::peer_process_id(std::os::fd::AsFd::as_fd(&stream))?;
    u32::try_from(pid).map_err(io::Error::other)
}
