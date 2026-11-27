// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Read-only cgroup v2 sizing. Resolve the process's hierarchy rather than
//! assuming /sys/fs/cgroup is its leaf. Walk only visible ancestors: a cgroup
//! namespace can hide stricter ancestors, so this is not an allocation guarantee.

use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf};

use super::CgroupMemoryStatus;
use crate::mount_table::unescape;

const PROC_LIMIT: u64 = 1024 * 1024;
const MAX_DEPTH: usize = 256;

#[derive(Debug)]
pub(super) struct Limits {
    pub high: Option<u64>,
    pub max: Option<u64>,
    /// Bytes left under the tightest visible high or max, after the usage of
    /// the group that sets it.
    pub headroom: Option<u64>,
    pub status: CgroupMemoryStatus,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            high: None,
            max: None,
            headroom: None,
            status: CgroupMemoryStatus::Incomplete,
        }
    }
}

pub(super) fn read() -> Limits {
    let Ok(cgroup) = read_bounded(Path::new("/proc/self/cgroup"), PROC_LIMIT) else {
        return Limits::default();
    };
    let Ok(mountinfo) = read_bounded(Path::new("/proc/self/mountinfo"), PROC_LIMIT) else {
        return Limits::default();
    };
    observe(&cgroup, &mountinfo, |path| read_bounded(path, 128))
}

fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    Ok(crate::file_io::read_bounded(path, limit)?)
}

pub(super) fn observe(
    cgroup: &[u8],
    mountinfo: &[u8],
    mut read: impl FnMut(&Path) -> io::Result<Vec<u8>>,
) -> Limits {
    let mut limits = Limits::default();
    let path = match unified_path(cgroup) {
        Ok(Some(path)) => path,
        Ok(None) => {
            limits.status = CgroupMemoryStatus::NotUnified;
            return limits;
        }
        Err(()) => return limits,
    };
    let Some((mount, mut leaf)) = locate(&path, mountinfo) else {
        return limits;
    };
    limits.status = if read(&leaf.join("cgroup.controllers")).is_ok() {
        CgroupMemoryStatus::Observed
    } else {
        // A migrated/deleted group must not look like an unrestricted root.
        CgroupMemoryStatus::Incomplete
    };
    loop {
        let mut local: Option<u64> = None;
        for (name, value) in [
            ("memory.high", &mut limits.high),
            ("memory.max", &mut limits.max),
        ] {
            match read(&leaf.join(name)) {
                Ok(bytes) => match parse_limit(&bytes) {
                    Ok(Some(limit)) => {
                        *value = Some(value.map_or(limit, |old| old.min(limit)));
                        local = Some(local.map_or(limit, |old| old.min(limit)));
                    }
                    Ok(None) => {}
                    Err(()) => limits.status = CgroupMemoryStatus::Incomplete,
                },
                // A root or a group with the controller disabled has no file.
                // Ancestors can still impose limits, so do not stop walking.
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => limits.status = CgroupMemoryStatus::Incomplete,
            }
        }
        // A limit binds the whole group it is set on, so what is left under it
        // is that group's usage subtracted; unread usage leaves the limit.
        if let Some(limit) = local {
            let current = read(&leaf.join("memory.current"))
                .ok()
                .and_then(|bytes| parse_limit(&bytes).ok().flatten())
                .unwrap_or(0);
            let left = limit.saturating_sub(current);
            limits.headroom = Some(limits.headroom.map_or(left, |old| old.min(left)));
        }
        if leaf == mount {
            break;
        }
        if !leaf.pop() || !leaf.starts_with(&mount) {
            limits.status = CgroupMemoryStatus::Incomplete;
            break;
        }
    }
    limits
}

fn unified_path(text: &[u8]) -> Result<Option<PathBuf>, ()> {
    let mut paths = text
        .split(|byte| *byte == b'\n')
        .filter_map(|line| line.strip_prefix(b"0::"));
    let Some(bytes) = paths.next() else {
        return Ok(None);
    };
    if paths.next().is_some() {
        return Err(());
    }
    clean_absolute(bytes).map(Some)
}

fn clean_absolute(bytes: &[u8]) -> Result<PathBuf, ()> {
    if bytes.contains(&0) || !bytes.starts_with(b"/") {
        return Err(());
    }
    // Reject traversal and ambiguous spelling instead of silently normalizing it.
    if bytes
        .split(|byte| *byte == b'/')
        .any(|part| part == b".." || part == b".")
    {
        return Err(());
    }
    let path = PathBuf::from(OsString::from_vec(bytes.to_vec()));
    if path.components().count() > MAX_DEPTH
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(());
    }
    Ok(path)
}

fn locate(path: &Path, text: &[u8]) -> Option<(PathBuf, PathBuf)> {
    let mut selected: Option<(usize, PathBuf, PathBuf)> = None;
    for line in text.split(|byte| *byte == b'\n') {
        let Some(separator) = line.windows(3).position(|window| window == b" - ") else {
            continue;
        };
        let left: Vec<_> = line[..separator].split(|byte| *byte == b' ').collect();
        let kind = line[separator + 3..].split(|byte| *byte == b' ').next();
        if left.len() < 6 || kind != Some(b"cgroup2".as_slice()) {
            continue;
        }
        let (Ok(root), Ok(mount)) = (
            clean_absolute(&unescape(left[3])),
            clean_absolute(&unescape(left[4])),
        ) else {
            continue;
        };
        let Ok(relative) = path.strip_prefix(&root) else {
            continue;
        };
        let depth = root.components().count();
        // Prefer the most complete visible hierarchy; a bind mount of a leaf
        // hides ancestors which the full mount may still let us inspect.
        if selected.as_ref().is_none_or(|(old, _, _)| depth < *old) {
            let leaf = mount.join(relative);
            selected = Some((depth, mount, leaf));
        }
    }
    selected.map(|(_, mount, leaf)| (mount, leaf))
}

fn parse_limit(bytes: &[u8]) -> Result<Option<u64>, ()> {
    let text = std::str::from_utf8(bytes).map_err(|_| ())?.trim();
    if text == "max" {
        return Ok(None);
    }
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    text.parse().map(Some).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    const MOUNT: &[u8] = b"1 2 0:1 / /cg rw shared:3 - cgroup2 cgroup rw\n";

    fn fixture(cgroup: &[u8], mount: &[u8], entries: &[(&str, &str)]) -> (Limits, Vec<PathBuf>) {
        let files: BTreeMap<_, _> = entries
            .iter()
            .map(|(p, v)| (PathBuf::from(p), v.as_bytes().to_vec()))
            .collect();
        let mut reads = Vec::new();
        let limits = observe(cgroup, mount, |path| {
            reads.push(path.to_owned());
            if path
                .file_name()
                .is_some_and(|name| name == "cgroup.controllers")
            {
                return Ok(Vec::new());
            }
            files
                .get(path)
                .cloned()
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        });
        (limits, reads)
    }

    #[test]
    fn nested_process_uses_strictest_visible_ancestor() {
        let (limits, reads) = fixture(
            b"0::/user/app\n",
            MOUNT,
            &[
                ("/cg/user/app/memory.max", "max\n"),
                ("/cg/user/memory.max", "4294967296\n"),
                ("/cg/memory.max", "8589934592\n"),
                ("/cg/user/memory.high", "2147483648\n"),
            ],
        );
        assert_eq!(limits.max, Some(4 * 1024 * 1024 * 1024));
        assert_eq!(limits.high, Some(2 * 1024 * 1024 * 1024));
        assert_eq!(limits.status, CgroupMemoryStatus::Observed);
        assert_eq!(reads.len(), 9);
        assert!(reads.iter().all(|path| path.starts_with("/cg")));
    }

    #[test]
    fn headroom_follows_tightest_ancestor() {
        let (limits, _) = fixture(
            b"0::/user/app\n",
            MOUNT,
            &[
                ("/cg/user/app/memory.max", "max\n"),
                ("/cg/user/memory.max", "4000\n"),
                ("/cg/user/memory.current", "3000\n"),
                ("/cg/memory.high", "9000\n"),
                ("/cg/memory.current", "8500\n"),
            ],
        );
        assert_eq!(limits.headroom, Some(500));
        assert_eq!(limits.status, CgroupMemoryStatus::Observed);
    }

    #[test]
    fn namespace_root_and_zero_limits_kept() {
        let (limits, reads) = fixture(b"0::/\n", MOUNT, &[("/cg/memory.max", "0")]);
        assert_eq!(limits.max, Some(0));
        assert_eq!(reads.len(), 4);
    }

    #[test]
    fn subtree_mount_stops_at_root() {
        let (limits, reads) = fixture(
            b"0::/parent/child\n",
            b"1 2 0:1 /parent /cg rw - cgroup2 none rw",
            &[("/cg/memory.max", "1234")],
        );
        assert_eq!(limits.max, Some(1234));
        assert_eq!(reads.len(), 6);
        assert_eq!(reads[1], Path::new("/cg/child/memory.high"));
    }

    #[test]
    fn full_hierarchy_beats_leaf_mount() {
        let mounts =
            b"1 2 0:1 /parent/child /bind rw - cgroup2 none rw\n2 3 0:1 / /cg rw - cgroup2 none rw";
        let (limits, reads) = fixture(
            b"0::/parent/child",
            mounts,
            &[("/cg/parent/memory.max", "123")],
        );
        assert_eq!(limits.max, Some(123));
        assert_eq!(reads.len(), 8);
        assert!(reads.iter().all(|path| path.starts_with("/cg")));
    }

    #[test]
    fn escaped_mount_names_resolve() {
        let (limits, reads) = fixture(
            b"0::/leaf",
            b"1 2 0:1 / /cg\\040dir rw - cgroup2 none rw",
            &[("/cg dir/memory.high", "9")],
        );
        assert_eq!(limits.high, Some(9));
        assert_eq!(reads[1], Path::new("/cg dir/leaf/memory.high"));
        let (_, reads) = fixture(b"0::/leaf", b"1 2 0:1 / /cg\xff rw - cgroup2 none rw", &[]);
        assert_eq!(
            reads[1],
            PathBuf::from(OsString::from_vec(b"/cg\xff/leaf/memory.high".to_vec()))
        );
    }

    #[test]
    fn bad_hierarchies_stay_bounded() {
        let (v1, reads) = fixture(b"4:memory:/user", MOUNT, &[]);
        assert_eq!(v1.status, CgroupMemoryStatus::NotUnified);
        assert!(reads.is_empty());
        for path in [
            b"0::relative".as_slice(),
            b"0::/../escape",
            b"0::/a/./b",
            b"0::/\n0::/two",
            b"0::/bad\0path",
        ] {
            let (limits, reads) = fixture(path, MOUNT, &[]);
            assert_eq!(limits.status, CgroupMemoryStatus::Incomplete);
            assert!(reads.is_empty());
        }
        let (limits, reads) = fixture(
            b"0::/other",
            b"1 2 0:1 /private /cg rw - cgroup2 none rw",
            &[],
        );
        assert_eq!(limits.status, CgroupMemoryStatus::Incomplete);
        assert!(reads.is_empty());
    }

    #[test]
    fn bad_limits_keep_known_ancestors() {
        let (limits, _) = fixture(
            b"0::/leaf",
            MOUNT,
            &[
                ("/cg/leaf/memory.max", "garbage"),
                ("/cg/memory.max", "200"),
            ],
        );
        assert_eq!(limits.status, CgroupMemoryStatus::Incomplete);
        assert_eq!(limits.max, Some(200));
        let limits = observe(b"0::/", MOUNT, |_| {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        });
        assert_eq!(limits.status, CgroupMemoryStatus::Incomplete);
        assert_eq!(limits.max, None);
    }

    #[test]
    fn vanished_leaf_stays_bounded() {
        let limits = observe(b"0::/gone", MOUNT, |_| {
            Err(io::Error::from(io::ErrorKind::NotFound))
        });
        assert_eq!(limits.status, CgroupMemoryStatus::Incomplete);
    }

    #[test]
    fn reads_stop_at_their_bound() {
        assert_eq!(
            read_bounded(Path::new("/proc/self/mountinfo"), 8)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            read_bounded(Path::new("/dev/zero"), 8).unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "an endless device is refused before any read"
        );
    }

    #[test]
    fn absent_controller_files_mean_unknown() {
        let (limits, _) = fixture(b"0::/leaf", MOUNT, &[]);
        assert_eq!(limits.status, CgroupMemoryStatus::Observed);
        assert_eq!(limits.max, None);
        assert_eq!(limits.high, None);
    }

    #[test]
    fn limit_parser_refuses_bad_values() {
        assert_eq!(parse_limit(b"max\n"), Ok(None));
        assert_eq!(
            parse_limit(b"9007199254740993\n"),
            Ok(Some(9_007_199_254_740_993))
        );
        assert_eq!(parse_limit(b"0"), Ok(Some(0)));
        for text in [
            "",
            "-1",
            "+1",
            "1e6",
            "1 2",
            "18446744073709551616",
            "MAX",
            "3 MB",
        ] {
            assert_eq!(parse_limit(text.as_bytes()), Err(()), "{text}");
        }
    }

    #[test]
    fn mount_parser_skips_foreign_lines() {
        let mounts = b"1 2 0:1 / /wrong rw - tmpfs none rw\n1 2 0:1 / /wrong\\000 rw - cgroup2 none rw\n1 2 0:1 / /cg rw - cgroup2 none rw";
        let (limits, reads) = fixture(b"0::/", mounts, &[("/cg/memory.max", "99")]);
        assert_eq!(limits.max, Some(99));
        assert_eq!(reads.len(), 4);
    }

    #[test]
    fn pathological_depth_is_rejected_without_io() {
        let path = format!("0::/{}", "x/".repeat(MAX_DEPTH + 1));
        let (limits, reads) = fixture(path.as_bytes(), MOUNT, &[]);
        assert_eq!(limits.status, CgroupMemoryStatus::Incomplete);
        assert!(reads.is_empty());
    }
}
