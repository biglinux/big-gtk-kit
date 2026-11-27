// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

#![allow(
    clippy::disallowed_methods,
    reason = "test harness reads /proc outside any UI application main loop"
)]

//! Boot and `/proc` readers for headless leak censuses.
//!
//! The `/proc` readers ([`vmrss_kib`], [`smaps_rollup_kib`], [`anon_rss_kib`])
//! are pure; the loop pump, app boot and pixel probe ([`pump`], [`pump_until`],
//! [`pump_frame`], [`painted_pixel`], [`disable_animations`], [`run_census_app`])
//! need a GTK display.

use big_relm4_components::layout::widget_container::subtree;
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::rc::Rc;
use std::sync::{OnceLock, mpsc};

use adw::prelude::*;
use gtk::glib;

// Low-level display probe kept private so widget tests cannot bypass the
// process-wide owner-thread runner.
fn gtk_ready() -> bool {
    if std::env::var_os("BIG_RUN_UI_SMOKE").is_none() {
        announce_ui_skip();
        return false;
    }
    // GTK records the thread that initialized it and panics on a second one, so
    // this must be the only `gtk::init` in a test binary. A test that calls it
    // directly wins or loses a race against this thread, and when it wins every
    // UI test in the binary fails with "the GTK harness did not run this test".
    gtk::init().is_ok() && gtk::gdk::Display::default().is_some()
}

type GtkTestJob = Box<dyn FnOnce() + Send + 'static>;

/// Run a GTK test, and fail if the harness was enabled but did not run it.
///
/// Without `BIG_RUN_UI_SMOKE` the body is skipped. With it, a harness that
/// did not start would otherwise turn every widget test into a silent
/// pass. [`run_gtk_test`] is for a body whose skipping does not matter.
///
/// # Panics
///
/// Panics when `BIG_RUN_UI_SMOKE` is set and the body did not run.
pub fn require_gtk_test(test: impl FnOnce() + Send + 'static) {
    let did_run = run_gtk_test(test);
    assert!(
        did_run || std::env::var_os("BIG_RUN_UI_SMOKE").is_none(),
        "BIG_RUN_UI_SMOKE is set but the GTK harness did not run this test"
    );
}

/// Run one GTK test body on the process-wide dedicated GTK thread.
///
/// Rust's test harness creates a fresh worker thread even with
/// `--test-threads=1`, while GTK requires every initialization and widget
/// access in one process to stay on the same thread. This runner serializes
/// explicitly requested UI smokes on one owner thread and otherwise skips the
/// closure without touching the developer's desktop.
///
/// Returns `true` when the closure ran. A panic inside the closure resumes on
/// the calling test thread so the normal test harness reports the failure.
/// The disposable display runner sets `BIG_RUN_UI_SMOKE`.
///
/// ```
/// use gtk::prelude::*;
///
/// big_testkit::harness::require_gtk_test(|| {
///     let button = gtk::Button::with_label("Test");
///     assert_eq!(button.label().as_deref(), Some("Test"));
/// });
/// ```
///
/// Prefer [`require_gtk_test`] in a test that must not pass vacuously.
///
/// # Panics
///
/// With the panic of `test`, and when the GTK test thread is gone.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "a test harness: its panics are the failures it reports"
)]
pub fn run_gtk_test(test: impl FnOnce() + Send + 'static) -> bool {
    let Some(sender) = gtk_test_runner() else {
        return false;
    };
    let (done_sender, done_receiver) = mpsc::channel();
    sender
        .send(Box::new(move || {
            let result = catch_unwind(AssertUnwindSafe(test));
            done_sender
                .send(result)
                .expect("return dedicated GTK test result");
        }))
        .expect("send work to dedicated GTK test thread");
    match done_receiver
        .recv()
        .expect("receive dedicated GTK test result")
    {
        Ok(()) => true,
        Err(payload) => resume_unwind(payload),
    }
}

#[expect(
    clippy::expect_used,
    reason = "a test harness: its panics are the failures it reports"
)]
fn gtk_test_runner() -> Option<mpsc::Sender<GtkTestJob>> {
    static GTK_TEST_RUNNER: OnceLock<Option<mpsc::Sender<GtkTestJob>>> = OnceLock::new();
    GTK_TEST_RUNNER
        .get_or_init(|| {
            let (ready_sender, ready_receiver) = mpsc::channel();
            std::thread::spawn(move || {
                if !gtk_ready() {
                    let _ = ready_sender.send(None);
                    return;
                }
                let (job_sender, job_receiver) = mpsc::channel::<GtkTestJob>();
                ready_sender
                    .send(Some(job_sender))
                    .expect("publish dedicated GTK test thread");
                while let Ok(job) = job_receiver.recv() {
                    job();
                }
            });
            ready_receiver
                .recv()
                .expect("initialize dedicated GTK test thread")
        })
        .clone()
}

/// The example module's library, `libbigexample_module.so`, beside this test
/// binary in Cargo's target directory. For the module tests of the programs
/// that load modules; nothing in this repository calls it.
///
/// # Panics
///
/// Panics when it is not built: a module test that returned early instead would
/// pass without loading anything. Build it first.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "a test harness: its panics are the failures it reports"
)]
pub fn example_module_library() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("the test binary has a path");
    exe.ancestors()
        .flat_map(|ancestor| {
            [
                ancestor.join("libbigexample_module.so"),
                ancestor.join("deps/libbigexample_module.so"),
            ]
        })
        .find(|candidate| candidate.is_file())
        .expect("libbigexample_module.so is built (cargo build -p bigexample-module)")
}

/// Resident set size (KiB) of this process from `/proc/self/status` `VmRSS`.
///
/// Advisory: a growing-but-bounded GTK cache can inflate it, so pair it with
/// the exact [`crate::leak::WidgetFinalizeCensus`] for teardown proofs.
#[must_use]
pub fn vmrss_kib() -> u64 {
    proc_kib_field("/proc/self/status", "VmRSS:")
}

/// One internally consistent sample from `/proc/<pid>/smaps_rollup`, in KiB.
///
/// Read the file once per sample. Reading each field separately can combine
/// values from different instants while the process is allocating or freeing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SmapsRollupKib {
    /// Total resident pages represented by the rollup.
    pub rss: u64,
    /// Proportional set size, including shared mappings proportionally.
    pub pss: u64,
    /// Proportional set size attributed to anonymous mappings.
    pub pss_anon: u64,
    /// Private dirty pages, which cannot be reclaimed from a backing file.
    pub private_dirty: u64,
    /// Anonymous resident memory reported by the kernel.
    pub anonymous: u64,
    /// Swapped pages represented by the rollup.
    pub swap: u64,
    /// Proportional swapped size when the kernel exposes it.
    pub swap_pss: u64,
}

impl SmapsRollupKib {
    fn parse(contents: &str) -> Self {
        let mut sample = Self::default();
        for line in contents.lines() {
            let Some((field, rest)) = line.split_once(':') else {
                continue;
            };
            let Some(value) = rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<u64>().ok())
            else {
                continue;
            };
            match field {
                "Rss" => sample.rss = value,
                "Pss" => sample.pss = value,
                "Pss_Anon" => sample.pss_anon = value,
                "Private_Dirty" => sample.private_dirty = value,
                "Anonymous" => sample.anonymous = value,
                "Swap" => sample.swap = value,
                "SwapPss" => sample.swap_pss = value,
                _ => {}
            }
        }
        sample
    }
}

/// Read all retention-relevant `smaps_rollup` fields for `pid` in one pass.
///
/// Returns `None` when the process disappeared or `/proc` is unavailable.
#[must_use]
pub fn smaps_rollup_kib(pid: u32) -> Option<SmapsRollupKib> {
    std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
        .ok()
        .map(|contents| SmapsRollupKib::parse(&contents))
}

/// Anonymous (heap/mmap/renderer) RSS in KiB for `pid`.
///
/// Kept for the existing slope censuses; new multi-metric probes should use
/// [`smaps_rollup_kib`] so PSS, Pss_Anon and Private_Dirty belong to the same
/// instant. Returns `0` when the process has exited.
#[must_use]
pub fn anon_rss_kib(pid: u32) -> u64 {
    smaps_rollup_kib(pid).map_or(0, |sample| sample.anonymous)
}

/// Hand freed arenas back to the kernel, so the next [`anon_rss_kib`] reading
/// is about retention rather than about glibc's high-water mark.
///
/// Without this a census that keeps nothing still reads as growth, and the
/// same binary disagrees with itself run to run. It does what
/// `MALLOC_TRIM_THRESHOLD_=0` would, without the environment variable.
///
/// glibc only; a no-op on any allocator without `malloc_trim`, which the example
/// binaries do not link. Call it between census cycles.
pub fn trim_allocator() {
    unsafe extern "C" {
        fn malloc_trim(pad: usize) -> std::ffi::c_int;
    }
    // SAFETY: `malloc_trim` takes a padding size and returns a flag; it has no
    // preconditions and cannot invalidate any live pointer.
    unsafe {
        malloc_trim(0);
    }
}

/// Read the KiB value of a `Key:` field from a `/proc` status-style file.
fn proc_kib_field(path: &str, field: &str) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| {
            contents
                .lines()
                .find_map(|line| line.strip_prefix(field))
                .and_then(|rest| rest.split_whitespace().next().map(str::to_owned))
        })
        .and_then(|kib| kib.parse().ok())
        .unwrap_or(0)
}

/// Drain every pending default-`MainContext` task (iteration cap 10_000).
///
/// A synchronous census loop blocks the main loop, so realize / map / close /
/// finalize would otherwise never run. Call it between census steps.
/// Needs a running GTK app.
pub fn pump() {
    let main_context = glib::MainContext::default();
    let mut guard = 0;
    while main_context.pending() && guard < 10_000 {
        main_context.iteration(false);
        guard += 1;
    }
}

/// Press `key` with `modifiers` on the key controllers of `widget` in
/// `phase` (every phase with `None`), as GTK delivers it: each in turn until
/// one takes it. Whether one did.
pub fn press_key(
    widget: &impl IsA<gtk::Widget>,
    phase: Option<gtk::PropagationPhase>,
    key: gtk::gdk::Key,
    modifiers: gtk::gdk::ModifierType,
) -> bool {
    widget
        .observe_controllers()
        .iter::<glib::Object>()
        .filter_map(Result::ok)
        .filter_map(|controller| controller.downcast::<gtk::EventControllerKey>().ok())
        .filter(|keys| phase.is_none_or(|phase| keys.propagation_phase() == phase))
        .any(|keys| keys.emit_by_name::<bool>("key-pressed", &[&key, &0_u32, &modifiers]))
}

/// Fail naming every part of `css` GTK refuses: a rule it cannot parse is
/// dropped without a word when the program runs. Needs a running GTK, as in
/// [`require_gtk_test`].
///
/// # Panics
///
/// When GTK reports a parsing error, with each one's place and message.
pub fn assert_css_parses(css: &str) {
    let refused = Rc::new(RefCell::new(Vec::<String>::new()));
    let provider = big_relm4_components::css::provider();
    let seen = Rc::clone(&refused);
    provider.connect_parsing_error(move |_, section, error| {
        seen.borrow_mut().push(format!("{section}: {error}"));
    });
    provider.load_from_string(css);
    let refused = refused.borrow();
    assert!(
        refused.is_empty(),
        "GTK refused part of the stylesheet: {refused:?}"
    );
}

/// A fresh `MainContext` for one test, which is never finalized.
///
/// A zbus or blocking-pool thread wakes the context's tasks through GLib, which
/// holds a reference to the context while it does. When the test let go of its
/// last one in that moment, the context was finalized on the waking thread, and
/// glib-rs aborted the process dropping a task that belongs to the test thread.
/// The context stays alive until the process exits instead.
#[must_use]
pub fn test_context() -> glib::MainContext {
    let context = glib::MainContext::new();
    std::mem::forget(context.clone());
    context
}

/// Run `test` with a [`test_context`] as this thread's default, where the
/// local futures it spawns run; `test` gets the context to turn or block on.
///
/// `cargo test` runs every test of a binary in one process, on threads of
/// their own: a callback one test leaves on the shared default context runs
/// on whichever test iterates it next, where glib-rs panics inside GLib's
/// dispatch and the process aborts.
///
/// # Panics
///
/// With the panic of `test`.
#[expect(
    clippy::expect_used,
    reason = "a test harness: its panics are the failures it reports"
)]
pub fn on_test_context<R>(test: impl FnOnce(&glib::MainContext) -> R) -> R {
    let context = test_context();
    context
        .with_thread_default(|| test(&context))
        .expect("a new context has no owner")
}

/// Turn off GTK animations so window close is synchronous under a headless
/// display (otherwise the close animation defers finalization and false-flags
/// a leak). No-op when GTK settings are not yet available.
pub fn disable_animations() {
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_enable_animations(false);
    }
}

/// How long [`pump_until`] waits for an event that does not come.
const PUMP_UNTIL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Drain the main loop until `is_ready` holds, or give up once 5 s have gone
/// by waiting with nothing to dispatch.
///
/// [`pump`] alone cannot prove a window mapped or focused: `iteration(false)`
/// returns as soon as no source is ready, and the compositor's configure event
/// for a freshly presented toplevel arrives after that. Returns whether the
/// condition held. Needs a running GTK app.
///
/// The time this thread spends dispatching does not count. The compositor
/// activates a freshly presented window once its first frame arrives, and that
/// frame's layout can take seconds of a debug build's CPU, which a suite
/// running on every core stretches past 5 s of wall time with no event late.
#[must_use]
pub fn pump_until(is_ready: impl Fn() -> bool) -> bool {
    let mut waited = std::time::Duration::ZERO;
    while !is_ready() {
        if waited >= PUMP_UNTIL_TIMEOUT {
            return false;
        }
        pump();
        let asleep = std::time::Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(5));
        waited += asleep.elapsed();
    }
    true
}

/// How many times `widget` maps from now on.
///
/// The compositor takes every popup down when another window becomes active,
/// which another test's window may do at any time, and a popup can open and
/// close again inside one [`pump`]. Polling `is_mapped` with [`pump_until`]
/// misses that opening; `pump_until(|| maps.get() == 1)` counts it.
#[must_use]
pub fn count_maps(widget: &impl IsA<gtk::Widget>) -> Rc<Cell<u32>> {
    let maps = Rc::new(Cell::new(0));
    widget.connect_map({
        let maps = Rc::clone(&maps);
        move |_| maps.set(maps.get() + 1)
    });
    maps
}

/// Run the main loop until `done` holds, sleeping between its events.
///
/// For a condition an event brings (a worker's answer, a timer, the
/// compositor), which may take longer than [`pump_until`]'s 5 s on a loaded
/// machine. There is no deadline: nextest's `slow-timeout` ends a test that
/// waits forever. A condition nothing on the main loop changes never holds.
pub fn wait_until(mut done: impl FnMut() -> bool) {
    let main_context = glib::MainContext::default();
    while !done() {
        main_context.iteration(true);
    }
}

/// Drain the main loop until `widget`'s window paints its next frame, or give
/// up as [`pump_until`] does. Returns whether it painted; `false` too for an
/// unrealized widget. A frame waits on the compositor, so a loop of [`pump`]
/// alone may never draw what it shows.
#[must_use]
pub fn pump_frame(widget: &impl IsA<gtk::Widget>) -> bool {
    let Some(clock) = widget.frame_clock() else {
        return false;
    };
    let painted = Rc::new(Cell::new(false));
    let handler = clock.connect_after_paint({
        let painted = Rc::clone(&painted);
        move |_| painted.set(true)
    });
    // A hidden widget queues no draw, and a clock with nothing to do never paints.
    clock.request_phase(gtk::gdk::FrameClockPhase::AFTER_PAINT);
    let ready = pump_until(|| painted.get());
    clock.disconnect(handler);
    ready
}

/// The straight RGBA pixel `widget` paints at its center.
///
/// The widget is rendered alone, through its window's renderer, so what is
/// behind it does not count. Needs a widget its window has laid out.
///
/// # Panics
///
/// Panics when the widget is not in a realized window.
#[must_use]
pub fn painted_pixel(widget: &impl IsA<gtk::Widget>) -> [u8; 4] {
    painted_pixel_at(widget, widget.width() / 2, widget.height() / 2)
}

/// The straight RGBA pixel `widget` paints at `x`, `y` in its own box, as
/// [`painted_pixel`] renders it; a point outside the box reads as transparent.
///
/// # Panics
///
/// Panics when the widget is not in a realized window.
#[must_use]
pub fn painted_pixel_at(widget: &impl IsA<gtk::Widget>, x: i32, y: i32) -> [u8; 4] {
    assert!(
        widget
            .native()
            .and_then(|native| native.renderer())
            .is_some(),
        "a widget in a realized window"
    );
    let Some(texture) = big_gtk_kit::capture::widget_texture(widget) else {
        return [0; 4]; // it paints nothing
    };
    let mut downloader = gtk::gdk::TextureDownloader::new(&texture);
    downloader.set_format(gtk::gdk::MemoryFormat::R8g8b8a8);
    let (pixels, stride) = downloader.download_bytes();
    if x < 0 || y < 0 || x >= texture.width() || y >= texture.height() {
        return [0; 4];
    }
    let index = |extent: i32| usize::try_from(extent).unwrap_or(0);
    let at = index(y) * stride + index(x) * 4;
    [pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]]
}

/// Whether `widget` or a widget under it has `role` and the accessible name
/// `label`.
///
/// The name is the `label` property assistive technology reads: a menu row
/// GTK names by a `labeled-by` relation to its text has none.
#[must_use]
pub fn has_named_descendant(
    widget: &impl IsA<gtk::Widget>,
    role: gtk::AccessibleRole,
    label: &str,
) -> bool {
    let widget = widget.as_ref();
    // A label holding a NUL names nothing.
    let Ok(wanted) = std::ffi::CString::new(label) else {
        return false;
    };
    subtree(widget)
        .filter(|widget| widget.accessible_role() == role)
        .any(|widget| {
            // SAFETY: the widget outlives the call, and the label property's
            // variadic value is a NUL-terminated string.
            let mismatch: Option<glib::GString> = unsafe {
                glib::translate::from_glib_full(gtk::ffi::gtk_test_accessible_check_property(
                    glib::translate::ToGlibPtr::to_glib_none(
                        widget.upcast_ref::<gtk::Accessible>(),
                    )
                    .0,
                    gtk::ffi::GTK_ACCESSIBLE_PROPERTY_LABEL,
                    wanted.as_ptr(),
                ))
            };
            mismatch.is_none()
        })
}

/// Whether assistive technology reads `widget`'s description as `text`: its
/// description property, with no described-by relation ahead of it.
#[must_use]
pub fn described_as(widget: &impl IsA<gtk::Widget>, text: &str) -> bool {
    // A description holding a NUL describes nothing.
    let Ok(wanted) = std::ffi::CString::new(text) else {
        return false;
    };
    let accessible: *mut gtk::ffi::GtkAccessible =
        glib::translate::ToGlibPtr::to_glib_none(widget.as_ref().upcast_ref::<gtk::Accessible>()).0;
    // SAFETY: the widget outlives both calls, and the description property's
    // variadic value is a NUL-terminated string.
    let (related, mismatch): (bool, Option<glib::GString>) = unsafe {
        (
            glib::translate::from_glib(gtk::ffi::gtk_test_accessible_has_relation(
                accessible,
                gtk::ffi::GTK_ACCESSIBLE_RELATION_DESCRIBED_BY,
            )),
            glib::translate::from_glib_full(gtk::ffi::gtk_test_accessible_check_property(
                accessible,
                gtk::ffi::GTK_ACCESSIBLE_PROPERTY_DESCRIPTION,
                wanted.as_ptr(),
            )),
        )
    };
    !related && mismatch.is_none()
}

/// Whether `widget` is labeled by `label` and by no other widget.
#[must_use]
pub fn labeled_only_by(widget: &impl IsA<gtk::Widget>, label: &impl IsA<gtk::Widget>) -> bool {
    let accessible = |widget: &gtk::Widget| -> *mut gtk::ffi::GtkAccessible {
        glib::translate::ToGlibPtr::to_glib_none(widget.upcast_ref::<gtk::Accessible>()).0
    };
    // SAFETY: both widgets outlive the call, and the relation's variadic value
    // is a NULL-terminated list of accessibles.
    let mismatch: Option<glib::GString> = unsafe {
        glib::translate::from_glib_full(gtk::ffi::gtk_test_accessible_check_relation(
            accessible(widget.as_ref()),
            gtk::ffi::GTK_ACCESSIBLE_RELATION_LABELLED_BY,
            accessible(label.as_ref()),
            std::ptr::null_mut::<gtk::ffi::GtkAccessible>(),
        ))
    };
    mismatch.is_none()
}

/// Whether `row` is labeled by one of its two title labels, as
/// `big_relm4_components::input::entry_row` builds it, and a screen
/// reader says its title once.
#[must_use]
pub fn named_once(row: &impl IsA<adw::EntryRow>) -> bool {
    let row = row.upcast_ref::<adw::EntryRow>();
    row.list_mnemonic_labels()
        .first()
        .is_some_and(|title| labeled_only_by(row, title))
}

thread_local! {
    /// Exit code the currently running [`run_census_app`] will return. A
    /// census body flags a leak with [`fail_census`].
    static CENSUS_EXIT_CODE: Cell<i32> = const { Cell::new(0) };
}

/// Flag the running census as failed so [`run_census_app`] returns
/// `exit_code` (`3` = leak, by convention) once the app quits.
pub fn fail_census(exit_code: i32) {
    CENSUS_EXIT_CODE.with(|code| code.set(exit_code));
}

/// Boot a minimal libadwaita census app, present one window, run `body`
/// against it on the first idle, then quit, returning the census exit code
/// (`0` clean, `3` when `body` called [`fail_census`]). Needs a display:
/// a private session gives it one.
///
/// # Panics
///
/// Resumes a panic from `body` after the application has quit. Also panics if
/// the application exits before activation runs the census body.
pub fn run_census_app<F>(app_id: &str, body: F) -> i32
where
    F: FnOnce(&adw::ApplicationWindow) + 'static,
{
    // A census opens every window it walks. Started from a shell inside the
    // person's session it would open them on their screen: GTK falls back to
    // the session's X display when the Wayland socket it is given is missing.
    if std::env::var_os("BIGLINUX_UI_SESSION_MODE").as_deref() != Some("headless".as_ref()) {
        eprintln!("{app_id}: opens windows only in an isolated session (tools/headless-gate.sh)");
        return 2;
    }
    CENSUS_EXIT_CODE.with(|code| code.set(0));

    let application = adw::Application::builder().application_id(app_id).build();

    let body_slot = Rc::new(RefCell::new(Some(body)));
    let body_slot_for_activate = Rc::clone(&body_slot);
    let panic_payload = Rc::new(RefCell::new(None));
    let panic_payload_for_activate = Rc::clone(&panic_payload);
    application.connect_activate(move |application| {
        disable_animations();
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .default_width(480)
            .default_height(320)
            .title("big-testkit census")
            .build();
        window.present();
        pump();

        let hold = application.hold();
        let body = body_slot_for_activate.borrow_mut().take();
        let application_for_idle = application.clone();
        let panic_payload = Rc::clone(&panic_payload_for_activate);
        glib::idle_add_local_once(move || {
            let _hold = hold; // released as this idle returns, after quit()
            if let Some(body) = body
                && let Err(payload) = catch_unwind(AssertUnwindSafe(|| body(&window)))
            {
                *panic_payload.borrow_mut() = Some(payload);
            }
            application_for_idle.quit();
        });
    });

    application.run_with_args::<&str>(&[]);
    if let Some(payload) = panic_payload.borrow_mut().take() {
        resume_unwind(payload);
    }
    assert!(
        body_slot.borrow().is_none(),
        "census application exited before running its body"
    );
    CENSUS_EXIT_CODE.with(Cell::get)
}

/// Say once that UI tests did not run.
///
/// A skipped body is reported by libtest as `ok`, so a run that executed none
/// of them looks exactly like a run that passed all of them.
fn announce_ui_skip() {
    use std::sync::atomic::{AtomicBool, Ordering};

    static SAID: AtomicBool = AtomicBool::new(false);
    if SAID.swap(true, Ordering::Relaxed) {
        return;
    }
    announce_skip(
        "UI tests were SKIPPED (BIG_RUN_UI_SMOKE is unset). They pass vacuously here.\n      \
         Run them with the repository's headless-gate.sh: headless-gate.sh cargo test <args>",
    );
}

/// Say, where it cannot be swallowed, that part of a passing test did not run.
///
/// Written to the process stderr descriptor rather than through `eprintln!`:
/// libtest captures Rust-level output per test and prints it only for
/// failures, so a skip said that way reads as a plain pass.
pub fn announce_skip(message: &str) {
    use std::io::Write as _;

    // SAFETY: descriptor 2 is the process stderr for the whole run; the handle
    // is leaked so dropping it never closes it.
    let mut stderr = std::mem::ManuallyDrop::new(unsafe {
        <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(2)
    });
    let _ = writeln!(stderr, "\nnote: {message}\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_is_inert_without_opt_in() {
        if std::env::var_os("BIG_RUN_UI_SMOKE").is_some() {
            return;
        }
        assert!(!run_gtk_test(|| panic!("unrequested GTK test ran")));
    }

    /// A sheet GTK takes passes; one with a property GTK does not know fails.
    #[test]
    fn refused_css_fails() {
        require_gtk_test(|| {
            assert_css_parses("label { color: red; }");
            let refused = catch_unwind(|| assert_css_parses("label { colr: red; }"));
            assert!(refused.is_err());
        });
    }

    #[test]
    fn census_panic_returns_to_harness() {
        require_gtk_test(|| {
            let result = catch_unwind(AssertUnwindSafe(|| {
                run_census_app("br.com.biglinux.testkit.panic", |_| {
                    panic!("expected census panic");
                });
            }));
            assert!(result.is_err());
        });
    }

    #[test]
    #[expect(
        clippy::disallowed_types,
        reason = "reruns this test binary to read its stderr as cargo test shows it"
    )]
    fn skip_survives_output_capture() {
        if std::env::var_os("BIG_TESTKIT_SKIP_CHILD").is_some() {
            announce_skip("SKIPPED the probe leg");
            return;
        }
        // A passing test under libtest's default capture, as `cargo test` runs it.
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", "harness::tests::skip_survives_output_capture"])
            .env("BIG_TESTKIT_SKIP_CHILD", "1")
            .output()
            .expect("rerun this test");
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("SKIPPED the probe leg"));
    }

    /// A dispatch longer than the whole wait is no waiting: the condition an
    /// event brings after it is still seen, as a window's first frame on a
    /// loaded machine is followed by its activation.
    #[test]
    fn long_dispatch_is_no_wait() {
        require_gtk_test(|| {
            let ready = Rc::new(Cell::new(false));
            let set = Rc::clone(&ready);
            glib::idle_add_local_once(move || {
                std::thread::sleep(PUMP_UNTIL_TIMEOUT + std::time::Duration::from_millis(100));
                glib::timeout_add_local_once(std::time::Duration::from_millis(20), move || {
                    set.set(true);
                });
            });
            assert!(pump_until(|| ready.get()));
        });
    }

    /// A widget that paints only a corner reads as unpainted at its center.
    #[test]
    fn painted_pixel_samples_widget_center() {
        require_gtk_test(|| {
            let display = gtk::gdk::Display::default().expect("a display");
            let provider = big_relm4_components::css::provider();
            provider.load_from_string(".painted-corner { background-color: black; }");
            #[expect(clippy::disallowed_methods, reason = "a test stacks its own sheet")]
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            let corner = gtk::Box::builder()
                .css_classes(["painted-corner"])
                .width_request(8)
                .height_request(8)
                .halign(gtk::Align::Start)
                .valign(gtk::Align::Start)
                .build();
            let probe = gtk::Box::builder().height_request(32).build();
            probe.append(&corner);
            let window = gtk::Window::builder()
                .default_width(64)
                .child(&probe)
                .build();
            window.present();
            assert!(pump_frame(&window));
            assert_eq!(
                (painted_pixel(&corner), painted_pixel(&probe)),
                ([0, 0, 0, 255], [0; 4])
            );
            window.destroy();
            gtk::style_context_remove_provider_for_display(&display, &provider);
        });
    }

    #[test]
    fn vmrss_of_self_is_nonzero() {
        assert!(vmrss_kib() > 0, "VmRSS of the test process should be > 0");
    }

    #[test]
    fn anon_rss_of_self_is_nonzero() {
        assert!(
            anon_rss_kib(std::process::id()) > 0,
            "Anonymous RSS of the test process should be > 0"
        );
    }

    #[test]
    fn smaps_rollup_reads_retention_fields() {
        let sample = SmapsRollupKib::parse(
            "Rss: 101 kB\nPss: 83 kB\nPss_Anon: 61 kB\nPrivate_Dirty: 47 kB\n\
             Anonymous: 59 kB\nSwap: 7 kB\nSwapPss: 3 kB\n",
        );
        assert_eq!(sample.rss, 101);
        assert_eq!(sample.pss, 83);
        assert_eq!(sample.pss_anon, 61);
        assert_eq!(sample.private_dirty, 47);
        assert_eq!(sample.anonymous, 59);
        assert_eq!(sample.swap, 7);
        assert_eq!(sample.swap_pss, 3);
    }

    #[test]
    fn smaps_rollup_of_self_is_sane() {
        let sample = smaps_rollup_kib(std::process::id()).expect("read self smaps_rollup");
        assert!(sample.rss > 0);
        assert!(sample.pss > 0);
        assert!(sample.pss <= sample.rss);
        assert!(sample.pss_anon <= sample.pss);
    }

    #[test]
    fn dead_pid_has_no_smaps_rollup() {
        assert_eq!(smaps_rollup_kib(u32::MAX), None);
    }

    #[test]
    fn dead_pid_anon_rss_is_zero() {
        assert_eq!(anon_rss_kib(u32::MAX), 0);
    }

    /// A thread waking a task of the context while the test lets go of it.
    #[test]
    fn foreign_wake_keeps_context() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        for _ in 0..200 {
            let context = test_context();
            let (wakers, woken) = mpsc::channel();
            context.block_on(async {
                glib::MainContext::ref_thread_default().spawn_local(std::future::poll_fn(
                    move |task| {
                        let _ = wakers.send(task.waker().clone());
                        std::task::Poll::<()>::Pending
                    },
                ));
                glib::timeout_future(std::time::Duration::ZERO).await;
            });
            let waker = woken.recv().expect("the task was polled");
            let stop = Arc::new(AtomicBool::new(false));
            let (started, waking_now) = mpsc::channel();
            let waking = {
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    waker.wake_by_ref();
                    let _ = started.send(());
                    while !stop.load(Ordering::Relaxed) {
                        waker.wake_by_ref();
                    }
                })
            };
            waking_now.recv().expect("the waking thread started");
            drop(context);
            stop.store(true, Ordering::Relaxed);
            waking.join().expect("the waking thread");
        }
    }
}
