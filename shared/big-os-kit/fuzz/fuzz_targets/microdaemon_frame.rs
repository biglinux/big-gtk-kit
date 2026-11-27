#![no_main]
// SPDX-License-Identifier: MIT
//! A frame as a peer on the daemon's socket sends it: length, then JSON.

use big_os_kit::microdaemon::{
    MicrodaemonCommand, MicrodaemonEvent, decode_microdaemon_frame, decode_microdaemon_json_frame,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = decode_microdaemon_frame(data);
    let _ = decode_microdaemon_json_frame::<MicrodaemonCommand>(data);
    let _ = decode_microdaemon_json_frame::<MicrodaemonEvent>(data);
});
