// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! A thumbnail decodes in a loader started at a lower priority than the
//! program asking for it. Skips when no sandbox works.

#![expect(clippy::expect_used, reason = "a failed fixture fails the test")]

use std::path::Path;

/// The nice value of `pid`, field 19 of proc_pid_stat(5).
fn nice_of(pid: &str) -> String {
    let stat = std::fs::read_to_string(Path::new("/proc").join(pid).join("stat")).expect("stat");
    let after_name = &stat[stat.rfind(')').expect("stat has a name") + 2..];
    after_name
        .split(' ')
        .nth(19 - 3)
        .expect("nice field")
        .to_owned()
}

/// The processes this test process has started and that still run.
fn children() -> Vec<String> {
    std::fs::read_dir("/proc/self/task")
        .expect("tasks")
        .flatten()
        .filter_map(|task| std::fs::read_to_string(task.path().join("children")).ok())
        .flat_map(|pids| {
            pids.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn thumbnail_loader_runs_niced() {
    let directory = std::env::temp_dir().join(format!("big-image-priority-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("test directory");
    let picture = directory.join("picture.png");
    let pixels = big_image::BigRgbaImage {
        rgba: [200, 40, 40, 255].repeat(8 * 8),
        width: 8,
        height: 8,
        cicp: None,
        opaque: true,
    };
    // Encoding starts a glycin process of its own, kept for the next request.
    std::fs::write(&picture, big_image::png_bytes(&pixels).expect("png")).expect("picture");
    let before = children();

    let decoded = big_testkit::harness::test_context().block_on(
        big_image::decode_rgba_sandboxed_async(&big_image::BigImageSpec::thumbnail(&picture, 4, 4)),
    );
    std::fs::remove_dir_all(&directory).expect("remove test directory");
    match decoded {
        Err(big_image::BigImageError::SandboxUnavailable) => {
            eprintln!("skip: no sandbox for glycin here");
            return;
        }
        decoded => assert_eq!(decoded.expect("decoded").width, 4),
    }

    // glycin keeps the loader running for its next request; the process the
    // decode started is the sandbox around it.
    let loaders: Vec<String> = children()
        .into_iter()
        .filter(|child| !before.contains(child))
        .collect();
    assert!(!loaders.is_empty(), "the loader is still running");
    for loader in loaders {
        assert_eq!(nice_of(&loader), "10", "loader process {loader}");
    }
}
