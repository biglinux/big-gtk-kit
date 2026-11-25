#![no_main]
// SPDX-License-Identifier: MIT
//! Exercise the private parser with supplied bytes; never read the real hierarchy.

use big_os_kit::system_memory::CgroupMemoryStatus;
use libfuzzer_sys::fuzz_target;

#[allow(
    dead_code,
    reason = "the target calls only the parser of the included module"
)]
#[path = "../../src/system_memory/cgroup.rs"]
mod cgroup;

fuzz_target!(|data: &[u8]| {
    let mut fields = data.splitn(3, |byte| *byte == b'|');
    let membership = fields.next().unwrap_or_default();
    let mounts = fields.next().unwrap_or_default();
    let value = fields.next().unwrap_or_default();
    let mut reads = 0;
    let limits = cgroup::observe(membership, mounts, |_| {
        reads += 1;
        assert!(reads <= 513, "ancestor traversal must remain bounded");
        Ok(value.to_vec())
    });
    assert_eq!(
        limits.high, limits.max,
        "both controls received identical bytes"
    );
});
