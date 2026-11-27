#![no_main]
// SPDX-License-Identifier: MIT
//! Parse only: hostile paths must never be opened by the fuzz harness.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use big_os_kit::pressure_protocol::PressureConfig;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut fields = data.splitn(2, |byte| *byte == b'|');
    let path = OsStr::from_bytes(fields.next().unwrap_or_default());
    let trigger = fields.next().map(OsStr::from_bytes);
    let _ = PressureConfig::from_values(Some(path), trigger);
});
