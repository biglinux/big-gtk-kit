// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Memory readings for sizing work and presenting the kernel's current estimate.
//! `total_memory_bytes` caches a sizing baseline, not a claim that
//! physical RAM cannot change (memory hotplug exists). Live UI uses `read_snapshot`.

use std::io::{self, Read};
use std::sync::OnceLock;

mod cgroup;

/// The cgroup reader over a membership line, a mount table and a control's
/// value the kernel did not produce; for `cargo fuzz` only. Every control
/// reads `value`, so both limits agree, and the walk up the tree stays bounded.
#[cfg(fuzzing)]
#[doc(hidden)]
pub fn fuzz_cgroup(membership: &[u8], mounts: &[u8], value: &[u8]) {
    let mut reads = 0;
    let limits = cgroup::observe(membership, mounts, |_| {
        reads += 1;
        assert!(reads <= 513, "ancestor traversal must remain bounded");
        Ok(value.to_vec())
    });
    assert_eq!(limits.high, limits.max);
}

const MEMINFO_LIMIT: usize = 64 * 1024;

/// Whether all visible cgroup v2 memory controls could be inspected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgroupMemoryStatus {
    /// No unified hierarchy was advertised. Legacy v1 limits are not inferred.
    NotUnified,
    /// All visible ancestor controls were read, absent, or explicitly unlimited.
    Observed,
    /// Topology or at least one control was unavailable/malformed. Not unlimited.
    Incomplete,
}

/// Capacity inputs for conservative sizing, not currently free/available RAM.
/// The limits may be shared with other processes; hidden namespace ancestors,
/// later migrations and runtime limit changes are not allocation guarantees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MemoryCapacity {
    /// Physical MemTotal, kept separate for UI presentation.
    pub physical_bytes: Option<u64>,
    /// Minimum finite memory.high across visible ancestors (a throttle boundary).
    pub cgroup_high_bytes: Option<u64>,
    /// Minimum finite memory.max across visible ancestors (a hard limit).
    pub cgroup_max_bytes: Option<u64>,
    /// Missing permissions/topology are distinguishable from unlimited controls.
    pub cgroup_status: CgroupMemoryStatus,
}

impl MemoryCapacity {
    /// Minimum known physical/high/max bound. It is not exclusive to this process
    /// and high is used for sizing, not labeled a hard RAM limit.
    #[must_use]
    pub(crate) fn observed_bytes(&self) -> Option<u64> {
        [
            self.physical_bytes,
            self.cgroup_high_bytes,
            self.cgroup_max_bytes,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Sizing input. An incomplete/unsupported hierarchy without any observed
    /// finite constraint is unknown, so consumers choose conservative defaults.
    #[must_use]
    pub(crate) fn sizing_bytes(&self) -> Option<u64> {
        if self.cgroup_status == CgroupMemoryStatus::Observed
            || self.cgroup_high_bytes.is_some()
            || self.cgroup_max_bytes.is_some()
        {
            self.observed_bytes()
        } else {
            None
        }
    }
}

/// Read current capacity constraints, with bounded reads and no subprocess.
/// Missing or invalid inputs remain unknown. [`effective_memory_bytes`] keeps
/// the first answer for the rest of the process.
#[must_use]
pub(crate) fn read_capacity() -> MemoryCapacity {
    let limits = cgroup::read();
    MemoryCapacity {
        physical_bytes: read_snapshot()
            .ok()
            .and_then(|snapshot| snapshot.total_bytes),
        cgroup_high_bytes: limits.high,
        cgroup_max_bytes: limits.max,
        cgroup_status: limits.status,
    }
}

/// The capacity adaptive budgets size from, read once per process. Unlike
/// [`total_memory_bytes`], it counts the visible cgroup v2 ancestors' limits.
#[must_use]
pub(crate) fn effective_memory_bytes() -> Option<u64> {
    static CAPACITY: OnceLock<Option<u64>> = OnceLock::new();
    *CAPACITY.get_or_init(|| read_capacity().sizing_bytes())
}

/// Values missing, malformed or unavailable in this reading are not zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemorySnapshot {
    /// Physical RAM the kernel reports as installed, when `MemTotal` was read.
    pub total_bytes: Option<u64>,
    /// The kernel's estimate for starting applications without swapping;
    /// unlike MemFree this includes reclaimable memory. It is not a quota.
    pub available_bytes: Option<u64>,
}

/// Stable session sizing baseline. Use `read_snapshot` to display live state.
#[must_use]
pub fn total_memory_bytes() -> Option<u64> {
    static TOTAL: OnceLock<Option<u64>> = OnceLock::new();
    *TOTAL.get_or_init(|| read_snapshot().ok()?.total_bytes)
}

/// Bytes that can be allocated now without swapping or meeting a visible
/// cgroup's memory.high or memory.max: the lesser of MemAvailable and the
/// tightest group's headroom. Live, so blocking: run outside the UI thread.
#[must_use]
pub fn allocatable_bytes() -> Option<u64> {
    let available = read_snapshot()
        .ok()
        .and_then(|snapshot| snapshot.available_bytes);
    [available, cgroup::read().headroom]
        .into_iter()
        .flatten()
        .min()
}

/// Read a bounded live snapshot without a subprocess. Run outside the UI thread.
///
/// # Errors
///
/// The I/O error of reading `/proc/meminfo`, and [`io::ErrorKind::InvalidData`]
/// when it passes the size limit or is not UTF-8.
pub fn read_snapshot() -> io::Result<MemorySnapshot> {
    let mut bytes = Vec::new();
    std::fs::File::open("/proc/meminfo")?
        .take((MEMINFO_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MEMINFO_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "meminfo exceeds the limit",
        ));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(parse_snapshot(text))
}

fn kib_field(text: &str, key: &str) -> Option<u64> {
    let mut matches = text.lines().filter_map(|line| line.strip_prefix(key));
    let value = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let mut fields = value.split_whitespace();
    let number = fields.next()?.parse::<u64>().ok()?;
    if fields.next() != Some("kB") || fields.next().is_some() {
        return None;
    }
    number.checked_mul(1024)
}

fn parse_snapshot(text: &str) -> MemorySnapshot {
    let total = kib_field(text, "MemTotal:").filter(|total| *total > 0);
    let available = kib_field(text, "MemAvailable:")
        .filter(|available| total.is_none_or(|total| *available <= total));
    MemorySnapshot {
        total_bytes: total,
        available_bytes: available,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_respects_control_boundaries() {
        let mut capacity = MemoryCapacity {
            physical_bytes: Some(64 << 30),
            cgroup_max_bytes: Some(4 << 30),
            cgroup_high_bytes: Some(2 << 30),
            cgroup_status: CgroupMemoryStatus::Observed,
        };
        assert_eq!(capacity.sizing_bytes(), Some(2 << 30));
        assert_eq!(capacity.physical_bytes, Some(64 << 30));
        capacity.cgroup_high_bytes = None;
        assert_eq!(capacity.sizing_bytes(), Some(4 << 30));
        capacity.physical_bytes = Some(1 << 30);
        assert_eq!(capacity.sizing_bytes(), Some(1 << 30));
    }

    #[test]
    fn unknown_and_zero_capacity_differ() {
        let mut capacity = MemoryCapacity {
            physical_bytes: Some(64 << 30),
            cgroup_max_bytes: None,
            cgroup_high_bytes: None,
            cgroup_status: CgroupMemoryStatus::Incomplete,
        };
        assert_eq!(capacity.sizing_bytes(), None);
        capacity.cgroup_status = CgroupMemoryStatus::NotUnified;
        assert_eq!(capacity.sizing_bytes(), None);
        capacity.cgroup_status = CgroupMemoryStatus::Observed;
        assert_eq!(capacity.sizing_bytes(), Some(64 << 30));
        capacity.physical_bytes = None;
        assert_eq!(capacity.sizing_bytes(), None);
        capacity.cgroup_max_bytes = Some(0);
        assert_eq!(capacity.sizing_bytes(), Some(0));
    }

    #[test]
    fn inaccessible_ancestor_keeps_upper_bound() {
        let capacity = MemoryCapacity {
            physical_bytes: Some(64 << 30),
            cgroup_max_bytes: Some(512 << 20),
            cgroup_high_bytes: None,
            cgroup_status: CgroupMemoryStatus::Incomplete,
        };
        assert_eq!(capacity.sizing_bytes(), Some(512 << 20));
    }

    #[test]
    fn available_memory_differs_from_free() {
        assert_eq!(
            parse_snapshot("MemTotal: 1024 kB\nMemFree: 4 kB\nMemAvailable: 600 kB\n"),
            MemorySnapshot {
                total_bytes: Some(1_048_576),
                available_bytes: Some(614_400)
            }
        );
        assert_eq!(
            parse_snapshot("MemTotal: 1024 kB\nMemFree: 4 kB\n").available_bytes,
            None
        );
    }

    #[test]
    fn bad_meminfo_fields_are_unknown() {
        assert_eq!(parse_snapshot("").total_bytes, None);
        for text in [
            "MemTotal: 0 kB",
            "MemTotal: bad kB",
            "MemTotal: 50 MB",
            "MemTotal: 18446744073709551615 kB",
            "MemTotal: 1 kB\nMemTotal: 2 kB",
        ] {
            assert_eq!(parse_snapshot(text).total_bytes, None, "{text}");
        }
        assert_eq!(
            parse_snapshot("MemTotal: 10 kB\nMemAvailable: 11 kB").available_bytes,
            None
        );
        assert_eq!(
            parse_snapshot("MemTotal: 10 kB\nMemAvailable: 0 kB").available_bytes,
            Some(0)
        );
    }

    #[test]
    fn total_memory_is_plausible() {
        if let Some(bytes) = total_memory_bytes() {
            assert!(bytes > 64 * 1024 * 1024, "implausibly small MemTotal");
        }
    }
}
