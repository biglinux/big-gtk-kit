// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! A private `dbus-daemon` for tests of code that must not start a service.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::subprocess::{BigSubprocessChild, BigSubprocessOutputMode, BigSubprocessSpec};

/// A bus that starts `name` on demand, as the system bus starts an activatable
/// service, by running `touch <dir>/started`: the file says whether anything
/// asked the bus to start it. `touch` never takes the name, so the start fails
/// after a second. Its configuration lives in `dir`; it listens on an
/// abstract socket, since a path under a long `TMPDIR` overflows `sun_path`.
///
/// The daemon and its address; `None` without `dbus-daemon` or when it did not
/// start. The caller kills the daemon by the child it gets back.
pub fn activating(dir: &Path, name: &str) -> Option<(BigSubprocessChild, String)> {
    // By absolute path: `/usr/bin` on Debian, `/usr/sbin` on Arch.
    let program = ["/usr/bin/dbus-daemon", "/usr/sbin/dbus-daemon"]
        .into_iter()
        .find(|path| Path::new(path).exists())?;
    let services = dir.join("services");
    std::fs::create_dir_all(&services).ok()?;
    std::fs::write(
        services.join(format!("{name}.service")),
        // The bus splits `Exec` into words as a shell would.
        format!(
            "[D-BUS Service]\nName={name}\nExec=/usr/bin/touch {}\n",
            crate::shell_word::shell_word(&dir.join("started").to_string_lossy())
        ),
    )
    .ok()?;
    static STARTED: AtomicU32 = AtomicU32::new(0);
    let socket = format!(
        "big-test-bus-{}-{}",
        std::process::id(),
        STARTED.fetch_add(1, Ordering::Relaxed)
    );
    let config = dir.join("bus.conf");
    std::fs::write(
        &config,
        format!(
            "<busconfig><type>session</type><listen>unix:abstract={socket}</listen>\
             <auth>EXTERNAL</auth><servicedir>{services}</servicedir>\
             <limit name=\"service_start_timeout\">1000</limit>\
             <policy context=\"default\"><allow send_destination=\"*\"/>\
             <allow receive_sender=\"*\"/><allow own=\"*\"/></policy></busconfig>",
            services = services
                .to_string_lossy()
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        ),
    )
    .ok()?;
    let mut daemon = BigSubprocessSpec::builder()
        .program(program)
        .arg(format!("--config-file={}", config.display()))
        .arg("--print-address")
        // The handle is the daemon itself, not a parent that forked and left.
        .arg("--nofork")
        // Drop does not run when the test binary is killed or aborts.
        .die_with_parent(true)
        .stdout(BigSubprocessOutputMode::Capture)
        .stderr(BigSubprocessOutputMode::Null)
        .build()
        .spawn()
        .ok()?;
    let mut line = String::new();
    let read = daemon
        .take_stdout()
        .map(|stdout| BufReader::new(stdout).read_line(&mut line));
    if !matches!(read, Some(Ok(_))) || line.trim().is_empty() {
        let _ = daemon.kill();
        let _ = daemon.wait();
        return None;
    }
    Some((daemon, line.trim().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The marker lands in `dir` and the bus reads its configuration even
    /// when the path holds a space, an ampersand or a quote.
    #[test]
    fn activation_marks_an_awkward_dir() {
        let dir = std::env::temp_dir().join(format!("big test&bus's-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (mut daemon, address) = activating(&dir, "org.example.Started").expect("dbus-daemon");
        let start = BigSubprocessSpec::builder()
            .program("dbus-send")
            .arg(format!("--bus={address}"))
            .args([
                "--print-reply",
                "--dest=org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus.StartServiceByName",
                "string:org.example.Started",
                "uint32:0",
            ])
            .stdout(BigSubprocessOutputMode::Null)
            .stderr(BigSubprocessOutputMode::Null)
            .build()
            .run();
        let started = dir.join("started").exists();
        let _ = daemon.kill();
        let _ = daemon.wait();
        std::fs::remove_dir_all(&dir).unwrap();
        // `touch` never takes the name, so the start itself fails.
        assert!(start.is_ok_and(|output| !output.status.success()));
        assert!(started, "the activation ran nothing in the directory");
    }
}
