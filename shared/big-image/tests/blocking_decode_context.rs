// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! A blocking decode drives glycin on a private main context that stops when the
//! decode returns. Work it hands to GIO's pool completes through that context, so
//! a hop still pending then keeps the task and the context, with its wakeup
//! descriptor, for the life of the process. A process of its own: the count of
//! descriptors would also see the contexts of tests running beside it.

#![expect(clippy::expect_used, reason = "a failed fixture fails the test")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use big_os_kit::session_resources::{self, MemoryClass};

fn eventfds() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("descriptors listed")
        .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
        .filter(|target| target.as_os_str() == "anon_inode:[eventfd]")
        .count()
}

#[test]
fn blocking_decodes_release_their_context() {
    let path = std::env::temp_dir().join(format!("big-image-{}-context.avif", std::process::id()));
    std::fs::write(&path, include_bytes!("fixtures/srgb.avif")).expect("fixture written");
    let decode = || {
        big_image::decode_rgba_sandboxed_cancellable(
            &big_image::BigImageSpec::bounded(&path, 2, 2),
            Arc::default(),
            session_resources::shared(),
            MemoryClass::Interactive,
            0,
        )
        .map(drop)
    };
    // The first decode starts what glycin keeps for the process.
    let first = decode();
    if matches!(first, Err(big_image::BigImageError::SandboxUnavailable)) {
        let _ = std::fs::remove_file(&path);
        eprintln!("skip: no sandbox for glycin here");
        return;
    }
    let before = eventfds();
    let rest = (0..20).try_for_each(|_| decode());
    // The thread that woke a decode's task last holds its context for the
    // moment the wake takes, which can outlast the decode, and lets the
    // context go then. A context kept for good is still there after 5 s.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut after = eventfds();
    while after > before && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        after = eventfds();
    }
    let _ = std::fs::remove_file(&path);
    first.expect("decodes");
    rest.expect("decodes");
    // A context the first decode left can still go during the rest: one fewer
    // is not a leak, one more is.
    assert!(
        after <= before,
        "eventfds kept by 20 decodes: {before} -> {after}"
    );
}
