// SPDX-License-Identifier: MIT

//! Test-only client process whose death the daemon must survive.

#![expect(
    clippy::expect_used,
    reason = "a test fixture: its failures fail the test that runs it"
)]

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use big_os_kit::microdaemon::{
    MicrodaemonBody, MicrodaemonCommand, MicrodaemonEvent, MicrodaemonFrame, MicrodaemonId,
    read_microdaemon_json_frame, write_microdaemon_json_frame,
};

fn main() -> std::io::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let socket_path = PathBuf::from(args.next().expect("socket path argument"));
    let job_id = args
        .next()
        .expect("job id argument")
        .to_string_lossy()
        .into_owned();
    let mut stream = UnixStream::connect(socket_path)?;
    write_microdaemon_json_frame(
        &mut stream,
        &MicrodaemonFrame::command(
            MicrodaemonId::Fileopsd,
            MicrodaemonCommand::AttachStream {
                job_id,
                after_sequence: None,
            },
        ),
    )
    .map_err(std::io::Error::other)?;
    loop {
        let frame: MicrodaemonFrame =
            read_microdaemon_json_frame(&mut stream).map_err(std::io::Error::other)?;
        let MicrodaemonBody::Event { event } = frame.body else {
            continue;
        };
        if matches!(event, MicrodaemonEvent::StreamFrame { .. }) {
            // Tells the test this client is attached and may be killed.
            std::io::stdout().write_all(b"x")?;
            std::io::stdout().flush()?;
            std::thread::sleep(Duration::from_secs(30));
            return Ok(());
        }
    }
}
