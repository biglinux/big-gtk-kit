// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The `READY=1` a `Type=notify` systemd unit waits for before it counts as
//! started.

use std::ffi::OsStr;
use std::io;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};

/// Tell the unit that started this process that it is ready, through
/// `$NOTIFY_SOCKET`. Nothing happens when no unit waits for it.
///
/// A failure is logged: the unit then fails its start at its timeout, which is
/// how systemd reports a service that never got ready.
pub fn notify() {
    let Some(socket) = std::env::var_os("NOTIFY_SOCKET") else {
        return;
    };
    if let Err(error) = send(&socket, b"READY=1") {
        log::warn!("the service manager was not told this process is ready: {error}");
    }
}

/// `socket` is a path, or an abstract name after `@` (sd_notify(3)).
fn send(socket: &OsStr, message: &[u8]) -> io::Result<()> {
    let address = match socket.as_bytes().strip_prefix(b"@") {
        Some(name) => SocketAddr::from_abstract_name(name)?,
        None => SocketAddr::from_pathname(socket)?,
    };
    UnixDatagram::unbound()?.send_to_addr(message, &address)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_reaches_notify_socket() {
        // In `/tmp`: a socket path must fit `sun_path` (108 bytes), and `TMPDIR` can be long.
        let dir =
            std::path::Path::new("/tmp").join(format!("service-ready-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notify");
        let by_path = UnixDatagram::bind(&path).unwrap();
        let name = format!("service-ready-{}", std::process::id());
        let by_name =
            UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(&name).unwrap()).unwrap();
        for (socket, receiver) in [
            (path.as_os_str().to_owned(), &by_path),
            (format!("@{name}").into(), &by_name),
        ] {
            send(&socket, b"READY=1").unwrap();
            receiver.set_nonblocking(true).unwrap();
            let mut buffer = [0; 16];
            let length = receiver.recv(&mut buffer).unwrap();
            assert_eq!(&buffer[..length], b"READY=1", "{}", socket.display());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
