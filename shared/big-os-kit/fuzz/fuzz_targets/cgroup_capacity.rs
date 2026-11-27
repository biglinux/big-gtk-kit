#![no_main]
// SPDX-License-Identifier: MIT
//! The cgroup reader over supplied bytes; it never reads the real hierarchy.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut fields = data.splitn(3, |byte| *byte == b'|');
    let membership = fields.next().unwrap_or_default();
    let mounts = fields.next().unwrap_or_default();
    let value = fields.next().unwrap_or_default();
    big_os_kit::system_memory::fuzz_cgroup(membership, mounts, value);
});
