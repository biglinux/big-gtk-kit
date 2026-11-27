// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Give memory back when the system asks, and only then.
//!
//! systemd's pressure-service protocol: PSI files signal POLLPRI, FIFOs and Unix
//! stream sockets signal readable. A watch owns its worker and at most one
//! pending main-context callback; dropping it stops both.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use big_gtk_kit::timer::OnceTimer;
use big_os_kit::pressure_protocol::{PressureConfig, PressureWatch};
use gio::prelude::*;

struct DeliveryState {
    active: AtomicBool,
    pending: Mutex<Option<glib::Source>>,
}

impl Default for DeliveryState {
    fn default() -> Self {
        Self {
            active: AtomicBool::new(true),
            pending: Mutex::new(None),
        }
    }
}

impl DeliveryState {
    fn stop(&self) {
        self.active.store(false, Ordering::Release);
        let source = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(source) = source {
            source.destroy();
        }
    }
}

/// Scoped watcher: dropping it stops polling and removes any pending UI release.
/// A callback already executing cannot be retroactively cancelled.
pub(crate) struct MemoryPressureWatch {
    _subscription: PressureWatch,
    delivery: Arc<DeliveryState>,
}

impl Drop for MemoryPressureWatch {
    fn drop(&mut self) {
        self.delivery.stop();
    }
}

fn schedule_release(
    state: &Arc<DeliveryState>,
    context: &glib::MainContext,
    release: impl Fn() + Send + 'static,
) {
    let mut pending = state.pending.lock().unwrap_or_else(|e| e.into_inner());
    if !state.active.load(Ordering::Acquire) || pending.is_some() {
        return;
    }
    // The source captures a weak owner, never a state -> source -> state cycle.
    let weak = Arc::downgrade(state);
    let source = glib::source::idle_source_new(
        Some("big-memory-release"),
        glib::Priority::DEFAULT,
        move || {
            if let Some(state) = weak.upgrade() {
                struct Reset(Arc<DeliveryState>);
                impl Drop for Reset {
                    fn drop(&mut self) {
                        self.0
                            .pending
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .take();
                    }
                }
                let _reset = Reset(state.clone());
                if state.active.load(Ordering::Acquire) {
                    release();
                }
            }
            glib::ControlFlow::Break
        },
    );
    // Keep attachment and registration atomic against stop(). No user code is
    // called here, even if GTK has not started owning its main context yet.
    source.attach(Some(context));
    *pending = Some(source);
}

/// Watch systemd memory pressure until the returned owner is dropped. Events are
/// coalesced into one pending main-context callback, without periodic polling
/// or touching GTK from the worker thread. Pressure is applied to `resources`
/// (the owner passed to hosted panes) and told to every other copy of this
/// crate in the process. `None` when another copy watches for the process
/// ([`watch_for_process`]).
///
/// # Errors
/// Reports invalid configuration, unavailable transports or thread creation.
pub(crate) fn watch_scoped_for(
    resources: Arc<big_os_kit::session_resources::SessionResources>,
    release: impl Fn() + Send + Clone + 'static,
) -> std::io::Result<Option<MemoryPressureWatch>> {
    if watched_by_another_copy() {
        return Ok(None);
    }
    let Some(config) = PressureConfig::from_environment()? else {
        return Ok(None);
    };
    let release = move || {
        if let Some(application) = process_application() {
            application.activate_action(PROCESS_PRESSURE_ACTION, None);
        }
        release();
    };
    start_watch(&config, glib::MainContext::default(), resources, release).map(Some)
}

/// The PID of the process whose copy of this crate called [`watch_for_process`].
const PROCESS_WATCHER: &str = big_os_kit::pressure_protocol::PROCESS_WATCHER;

/// Whether this copy called [`watch_for_process`].
static WATCHES_FOR_PROCESS: AtomicBool = AtomicBool::new(false);

/// Make this copy of the crate the one that watches memory pressure for the
/// whole process.
///
/// Each module library links a copy of its own, and each copy's session
/// runtime would start a watcher thread on the same pressure file. A copy that
/// finds this process's PID in `PROCESS_WATCHER` starts none, and hears of
/// pressure through `PROCESS_PRESSURE_ACTION` instead, from the main loop.
///
/// # Safety
///
/// No other thread may be running: it sets an environment variable.
pub unsafe fn watch_for_process() {
    WATCHES_FOR_PROCESS.store(true, Ordering::Relaxed);
    // SAFETY: the caller runs this before any other thread exists.
    unsafe { std::env::set_var(PROCESS_WATCHER, std::process::id().to_string()) };
}

/// Whether another copy of this crate watches for this process. A child
/// that inherited the variable reads another PID in it and watches for itself.
fn watched_by_another_copy() -> bool {
    !WATCHES_FOR_PROCESS.load(Ordering::Relaxed)
        && std::env::var(PROCESS_WATCHER).is_ok_and(|pid| pid == std::process::id().to_string())
}

pub(crate) fn start_watch(
    config: &PressureConfig,
    context: glib::MainContext,
    resources: Arc<big_os_kit::session_resources::SessionResources>,
    release: impl Fn() + Send + Clone + 'static,
) -> std::io::Result<MemoryPressureWatch> {
    let delivery = Arc::new(DeliveryState::default());
    let worker_delivery = delivery.clone();
    let subscription = PressureWatch::start(config, move || {
        // Admission reacts immediately even if the GTK loop is busy. No cache
        // or GTK object is touched from this worker.
        resources.note_pressure();
        schedule_release(&worker_delivery, &context, release.clone());
    })?;
    Ok(MemoryPressureWatch {
        _subscription: subscription,
        delivery,
    })
}

/// Hand back what the allocator is holding but nothing is using.
///
/// Whichever allocator the executable linked: this crate is a library and does
/// not choose one. So it asks the process what it has rather than linking
/// either.
///
/// This frees only pages the allocator already holds free. It does not free live
/// objects, and does not flush other threads' per-thread caches, so it is the
/// last step after [`release_registered`] has dropped the reconstructible
/// caches. It does not replace dropping them, and it cannot reclaim resident
/// memory that is still referenced.
pub fn release_allocator_memory() {
    // jemalloc: purge every arena. `MALLCTL_ARENAS_ALL` is 4096.
    if let Some(mallctl) = dynamic_symbol(c"mallctl") {
        // SAFETY: the signature is jemalloc's documented one, and a read-only
        // call with null buffers asks for no output.
        let mallctl: Mallctl =
            unsafe { std::mem::transmute::<*mut libc::c_void, Mallctl>(mallctl) };
        // SAFETY: as above; the name is a NUL-terminated literal.
        let code = unsafe {
            mallctl(
                c"arena.4096.purge".as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        log::debug!("memory pressure: jemalloc arena purge returned {code}");
        return;
    }
    // glibc: return the free arena pages it retains by default.
    // SAFETY: malloc_trim only releases pages nothing is using, and is
    // thread-safe.
    let trimmed = unsafe { libc::malloc_trim(0) };
    log::debug!("memory pressure: malloc_trim returned {trimmed}");
}

/// One registered cache-release callback, with its liveness kept separate from
/// its purge.
///
/// `live` answers "is the owner still there?" without freeing anything, so the
/// list can drop a closed window's entry the moment another registers, without
/// waiting for the next pressure event to run every purge. `purge` does the freeing, and
/// only runs on real pressure.
struct ReleaseHook {
    live: Box<dyn Fn() -> bool>,
    purge: Box<dyn FnMut()>,
}

thread_local! {
    /// Callbacks that free reconstructible caches when the system is short on
    /// memory. Registered on the main thread and run on the main thread, so they
    /// may touch widgets and the `!Send` caches those widgets own.
    ///
    /// Dead entries are pruned by their `live` check on every registration, so
    /// the list stays bounded by live owners between pressure events, not one
    /// dead closure per window ever opened.
    static RELEASE_HOOKS: std::cell::RefCell<Vec<ReleaseHook>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The action every copy of this crate answers, on the process's application.
///
/// In the shared host each module links its own copy of this crate, with its
/// own `RELEASE_HOOKS`. The host released its own copy when a window closed and
/// no other: the shell's icon textures and the file browser's thumbnails and
/// folder models waited for a system-pressure signal a desktop with spare RAM
/// never gets. libgio is one per process, and so is its default application, so
/// the first copy adds this action and every copy connects its own hooks to it.
const PROCESS_RELEASE_ACTION: &str = "big-release-caches";

/// The action that tells every copy's resource owner the system is short on
/// memory, from the one copy that watches ([`watch_for_process`]).
const PROCESS_PRESSURE_ACTION: &str = "big-memory-pressure";

/// The application this copy answers the process actions on, joining it as
/// soon as there is one, or `None` to act on this copy alone.
///
/// One handler per copy, on the thread that joined: the handler runs the hooks of
/// the thread that activates the action, so a second handler would run them
/// twice, and a hook registered on another thread is not this handler's to run.
fn process_application() -> Option<gio::Application> {
    static JOINED: Mutex<Option<(std::thread::ThreadId, usize)>> = Mutex::new(None);
    let application = gio::Application::default()?;
    let here = (std::thread::current().id(), application.as_ptr() as usize);
    {
        let mut joined = JOINED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match *joined {
            Some(already) if already == here => return Some(application),
            Some(_) => return None,
            None => *joined = Some(here),
        }
    }
    // Unlocked: adding an action emits `action-added`, whose handlers may
    // register a cache and come back here.
    let handlers: [(&str, fn()); 2] = [
        (PROCESS_RELEASE_ACTION, purge_this_copy),
        (PROCESS_PRESSURE_ACTION, || {
            big_os_kit::session_resources::shared().note_pressure();
        }),
    ];
    for (name, handler) in handlers {
        match application
            .lookup_action(name)
            .and_downcast::<gio::SimpleAction>()
        {
            Some(action) => {
                action.connect_activate(move |_, _| handler());
            }
            // Handled before it is added: adding it emits `action-added`,
            // whose handlers may already activate it.
            None => {
                let action = gio::SimpleAction::new(name, None);
                action.connect_activate(move |_, _| handler());
                application.add_action(&action);
            }
        }
    }
    Some(application)
}

/// Drop entries whose owner is gone, then push a new one. The liveness check
/// frees nothing, so pruning here never triggers a purge as a side effect.
fn push_pruned(hook: ReleaseHook) {
    let _ = process_application();
    // Expired captures can run arbitrary destructors, including registration of
    // another cache. Do not drop them while the registry RefCell is borrowed.
    let mut hooks = RELEASE_HOOKS.take();
    hooks.retain(|entry| (entry.live)());
    RELEASE_HOOKS.with_borrow_mut(|added| {
        hooks.append(added);
        hooks.push(hook);
        *added = hooks;
    });
}

thread_local! {
    static NAMED_RELEASES: std::cell::RefCell<std::collections::BTreeSet<&'static str>> =
        const { std::cell::RefCell::new(std::collections::BTreeSet::new()) };
}

/// Register one process-lifetime cache by stable framework key. Several linked
/// products may request it, but pressure invokes the cache purge only once.
/// Per-window caches must use [`register_release_for`] instead.
/// Returns true only for the first registration on the owner thread.
pub fn register_release_once(key: &'static str, purge: impl FnMut() + 'static) -> bool {
    let inserted = NAMED_RELEASES.with_borrow_mut(|keys| keys.insert(key));
    if inserted {
        register_release(purge);
    }
    inserted
}

/// Run this callback, on the main thread, each time the system reports memory
/// pressure, before the allocator hands its own free pages back.
///
/// For a process-lifetime cache: this stays registered for the life of the
/// process. A per-window cache should use [`register_release_for`] instead, so
/// its entry is dropped when the window is.
///
/// Dropping reconstructible caches (decoded thumbnails, a preview image,
/// anything held only to be quick) under pressure is what turns their pages
/// into the free pages [`release_allocator_memory`] returns. Call during
/// main-thread setup.
pub(crate) fn register_release(purge: impl FnMut() + 'static) {
    push_pruned(ReleaseHook {
        live: Box::new(|| true),
        purge: Box::new(purge),
    });
}

/// Register a per-owner cache-release callback that drops itself once `owner` is
/// gone.
///
/// The entry holds only a [`Weak`](std::rc::Weak): it neither keeps the owner
/// alive nor lingers after it. `purge` runs with the upgraded owner on pressure;
/// once the owner is dropped, the entry is removed at the next registration
/// without ever calling `purge`. This is the shape a window or pane wants:
/// register on creation, forget about deregistering.
pub fn register_release_for<T: 'static>(
    owner: &std::rc::Rc<T>,
    mut purge: impl FnMut(&T) + 'static,
) {
    let live_weak = std::rc::Rc::downgrade(owner);
    let purge_weak = std::rc::Rc::downgrade(owner);
    push_pruned(ReleaseHook {
        live: Box::new(move || live_weak.strong_count() > 0),
        purge: Box::new(move || {
            if let Some(owner) = purge_weak.upgrade() {
                purge(&owner);
            }
        }),
    });
}

/// Free every registered cache, then hand the allocator's free pages back.
///
/// This is what the session runtime runs on pressure. The order matters: the caches have
/// to drop before the trim, or the trim finds nothing new to return.
pub fn release_registered() {
    match process_application() {
        // Every copy in the process, this one included.
        Some(application) => application.activate_action(PROCESS_RELEASE_ACTION, None),
        None => purge_this_copy(),
    }
    release_allocator_memory();
}

/// Run this copy's hooks.
fn purge_this_copy() {
    // Take the list out of the cell before running anything: a purge may drop
    // the last reference to something whose own `Drop` calls back in here, and a
    // borrow held across that is a `BorrowMutError`, fatal in a GTK callback.
    let mut hooks = RELEASE_HOOKS.take();
    // Drop the dead, purge the living.
    hooks.retain(|entry| (entry.live)());
    for entry in &mut hooks {
        (entry.purge)();
    }
    // Keep any hooks registered while these ran, and put the survivors back.
    RELEASE_HOOKS.with_borrow_mut(|added| {
        hooks.append(added);
        *added = hooks;
    });
}

/// Release now, and once more after the closed surface has finished going.
///
/// A closed window's widgets finalize over the next turns of the main loop,
/// and what they free stays in the main thread's jemalloc cache, which only
/// that thread's own allocations trim. So the cache is flushed with
/// `thread.idle` once the frees are done; closing several windows in a row
/// resets the wait, so it runs once.
pub fn release_when_settled() {
    release_registered();
    thread_local! {
        static SETTLING: OnceTimer = OnceTimer::new();
    }
    SETTLING.with(|settling| {
        settling.arm(std::time::Duration::from_secs(5), || {
            if let Some(mallctl) = dynamic_symbol(c"mallctl") {
                // SAFETY: jemalloc's documented signature, as in
                // `release_allocator_memory`; `thread.idle` takes and returns nothing.
                let mallctl: Mallctl =
                    unsafe { std::mem::transmute::<*mut libc::c_void, Mallctl>(mallctl) };
                // SAFETY: a NUL-terminated name and null buffers.
                unsafe {
                    mallctl(
                        c"thread.idle".as_ptr(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        0,
                    );
                }
            }
            release_allocator_memory();
        })
    });
}

type Mallctl = unsafe extern "C" fn(
    *const libc::c_char,
    *mut libc::c_void,
    *mut usize,
    *mut libc::c_void,
    usize,
) -> libc::c_int;

/// A symbol if this process has one, without linking against it.
fn dynamic_symbol(name: &std::ffi::CStr) -> Option<*mut libc::c_void> {
    // SAFETY: RTLD_DEFAULT searches the process's own symbols; a missing name
    // returns null rather than failing.
    let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    (!symbol.is_null()).then_some(symbol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    fn hook_count() -> usize {
        RELEASE_HOOKS.with_borrow(Vec::len)
    }

    #[test]
    fn pruning_drops_captures_outside_borrow() {
        struct RegisterOnDrop(Rc<Cell<bool>>);
        impl Drop for RegisterOnDrop {
            fn drop(&mut self) {
                let called = self.0.clone();
                register_release(move || called.set(true));
            }
        }
        RELEASE_HOOKS.with_borrow_mut(Vec::clear);
        let owner = Rc::new(());
        let called = Rc::new(Cell::new(false));
        let capture = RegisterOnDrop(called.clone());
        register_release_for(&owner, move |_| {
            let _keep = &capture;
        });
        drop(owner);
        register_release(|| {});
        assert_eq!(
            hook_count(),
            2,
            "new and destructor-registered hooks survive"
        );
        release_registered();
        assert!(called.get());
        RELEASE_HOOKS.with_borrow_mut(Vec::clear);
    }

    #[test]
    fn shared_cache_is_purged_once() {
        RELEASE_HOOKS.with_borrow_mut(Vec::clear);
        NAMED_RELEASES.with_borrow_mut(std::collections::BTreeSet::clear);
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        for expected in [true, false, false] {
            let count = calls.clone();
            assert_eq!(
                register_release_once("test.cache", move || count.set(count.get() + 1)),
                expected
            );
        }
        assert_eq!(hook_count(), 1);
        release_registered();
        assert_eq!(calls.get(), 1);
        assert_eq!(std::rc::Rc::strong_count(&calls), 2);
        RELEASE_HOOKS.with_borrow_mut(Vec::clear);
        NAMED_RELEASES.with_borrow_mut(std::collections::BTreeSet::clear);
        assert_eq!(std::rc::Rc::strong_count(&calls), 1);
    }

    #[test]
    fn dead_owner_pruned_without_purging() {
        RELEASE_HOOKS.with_borrow_mut(Vec::clear);
        let purged = Rc::new(Cell::new(0));
        let owner = Rc::new(());
        register_release_for(&owner, {
            let purged = Rc::clone(&purged);
            move |()| purged.set(purged.get() + 1)
        });
        assert_eq!(hook_count(), 1);

        // The owner goes away, as a closed window's registry does.
        drop(owner);
        // A later registration prunes the dead entry, and pruning must never
        // run its purge, which is the whole reason liveness is kept separate.
        let live_owner = Rc::new(());
        register_release_for(&live_owner, |()| {});
        assert_eq!(hook_count(), 1, "the dead owner's entry is gone");
        assert_eq!(purged.get(), 0, "pruning must not run the purge");
        RELEASE_HOOKS.with_borrow_mut(Vec::clear);
    }

    #[test]
    fn process_hook_runs_every_event() {
        RELEASE_HOOKS.with_borrow_mut(Vec::clear);
        let ran = Rc::new(Cell::new(0));
        register_release({
            let ran = Rc::clone(&ran);
            move || ran.set(ran.get() + 1)
        });
        release_registered();
        assert_eq!(ran.get(), 1);
        release_registered();
        assert_eq!(ran.get(), 2, "a process-lifetime hook runs each time");
        RELEASE_HOOKS.with_borrow_mut(Vec::clear);
    }

    #[test]
    fn reentrant_registration_during_release_survives() {
        RELEASE_HOOKS.with_borrow_mut(Vec::clear);
        let added_ran = Rc::new(Cell::new(false));
        let owner = Rc::new(());
        register_release_for(&owner, {
            let added_ran = Rc::clone(&added_ran);
            move |()| {
                // Re-entering registration from inside a running purge would hit
                // BorrowMutError if the list stayed borrowed. `release_registered`
                // takes the list out first, so this borrow is free.
                register_release({
                    let added_ran = Rc::clone(&added_ran);
                    move || added_ran.set(true)
                });
            }
        });
        release_registered();
        assert!(
            !added_ran.get(),
            "the newly added hook waits for the next pass"
        );
        release_registered();
        assert!(added_ran.get());
        RELEASE_HOOKS.with_borrow_mut(Vec::clear);
    }

    #[test]
    fn bursts_queue_one_deferred_release() {
        let context = big_testkit::harness::test_context();
        let state = Arc::new(DeliveryState::default());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for _ in 0..1000 {
            let calls = calls.clone();
            schedule_release(&state, &context, move || {
                calls.fetch_add(1, Ordering::Relaxed);
            });
        }
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(context.iteration(false));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(!context.pending());
        let more = calls.clone();
        schedule_release(&state, &context, move || {
            more.fetch_add(1, Ordering::Relaxed);
        });
        context.iteration(false);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn shutdown_destroys_pending_source() {
        let context = big_testkit::harness::test_context();
        let state = Arc::new(DeliveryState::default());
        let capture = Arc::new(());
        let weak = Arc::downgrade(&capture);
        schedule_release(&state, &context, move || {
            let _ = &capture;
            panic!("stopped");
        });
        state.stop();
        assert!(weak.upgrade().is_none());
        assert!(!context.pending());
        schedule_release(&state, &context, || panic!("cannot reschedule after stop"));
        assert!(!context.pending());
    }

    #[test]
    fn release_runs_without_pending_mutex() {
        let context = big_testkit::harness::test_context();
        let state = Arc::new(DeliveryState::default());
        let running = state.clone();
        schedule_release(&state, &context, move || {
            assert!(running.pending.try_lock().is_ok());
        });
        context.iteration(false);
        assert!(state.pending.lock().unwrap().is_none());
    }

    #[test]
    fn stopped_watch_skips_dispatching_release() {
        let context = big_testkit::harness::test_context();
        let state = Arc::new(DeliveryState::default());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = calls.clone();
        schedule_release(&state, &context, move || {
            counted.fetch_add(1, Ordering::Relaxed);
        });
        // A stop on another thread that lands after the source began to run
        // has only cleared `active` by the time the callback reads it.
        state.active.store(false, Ordering::Release);
        assert!(context.iteration(false));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    /// Set in the child [`run_alone`] starts.
    const CHILD_RUN: &str = "BIG_APP_KIT_CHILD_PRESSURE";

    /// Run `test` again in a child process with `variables`, for a test of
    /// state the whole process shares: the environment, the default application.
    fn run_alone(test: &str, variables: &[(&str, &str)]) {
        let mut child = big_os_kit::subprocess::BigSubprocessSpec::builder()
            .program(std::env::current_exe().expect("test binary"))
            .args(["--exact", test, "--quiet"])
            .env_inherit(false)
            .env(CHILD_RUN, "1");
        for (name, value) in variables {
            child = child.env(name, value);
        }
        let out = child
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .run()
            .expect("child run");
        assert!(out.status.success(), "{}", out.stdout_lossy());
    }

    /// A module's copy of the crate starts no second watcher thread on the
    /// pressure file the process's watcher already polls.
    #[test]
    fn module_copy_starts_no_watcher() {
        if std::env::var_os(CHILD_RUN).is_none() {
            // A file that does not exist: a watcher that started would fail to open it.
            run_alone(
                "memory_pressure::tests::module_copy_starts_no_watcher",
                &[("MEMORY_PRESSURE_WATCH", "/nonexistent/big-pressure")],
            );
            return;
        }
        // SAFETY: the child runs this one test, on the only thread that
        // touches the environment.
        unsafe { std::env::set_var(PROCESS_WATCHER, std::process::id().to_string()) };
        let resources = big_os_kit::session_resources::SessionResources::new(
            big_os_kit::session_resources::ResourceLimits::for_capacity(None, 2),
        );
        assert!(matches!(watch_scoped_for(resources, || {}), Ok(None)));
    }

    /// The process actions run the hooks of the one thread that joined them:
    /// a second thread gets no application to run them twice through.
    #[test]
    fn second_thread_joins_no_application() {
        if std::env::var_os(CHILD_RUN).is_none() {
            run_alone(
                "memory_pressure::tests::second_thread_joins_no_application",
                &[],
            );
            return;
        }
        let application = gio::Application::new(None, gio::ApplicationFlags::NON_UNIQUE);
        application.set_default();
        assert!(process_application().is_some());
        let joined_elsewhere = std::thread::spawn(|| process_application().is_some())
            .join()
            .expect("the second thread");
        assert!(!joined_elsewhere);
    }

    /// A handler of the application's `action-added` that registers a cache
    /// while the process actions are added gets through.
    #[test]
    fn action_observer_registers_cache() {
        if std::env::var_os(CHILD_RUN).is_none() {
            run_alone(
                "memory_pressure::tests::action_observer_registers_cache",
                &[],
            );
            return;
        }
        let application = gio::Application::new(None, gio::ApplicationFlags::NON_UNIQUE);
        application.set_default();
        application.connect_action_added(None, |_, _| register_release(|| {}));
        assert!(process_application().is_some());
    }

    #[test]
    fn pressure_reaches_owner_before_ui() {
        use big_os_kit::session_resources::{
            MemoryClass, MemoryDomain, ResourceLimits, SessionResources,
        };
        use std::io::Write;
        use std::os::unix::{fs::DirBuilderExt, net::UnixListener};
        // In `/tmp`: a socket path must fit `sun_path` (108 bytes), and `TMPDIR` can be long.
        let dir = std::path::Path::new("/tmp").join(format!(
            "big-app-kit-pressure-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let path = dir.join("notify");
        let listener = UnixListener::bind(&path).unwrap();
        let config = PressureConfig::from_values(Some(path.as_os_str()), None)
            .unwrap()
            .unwrap();
        let context = big_testkit::harness::test_context();
        let owner = SessionResources::new(ResourceLimits::for_capacity(None, 2));
        let unaffected = SessionResources::new(ResourceLimits::for_capacity(None, 2));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let callback_calls = calls.clone();
        let watch = start_watch(&config, context.clone(), owner.clone(), move || {
            callback_calls.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap();
        let (mut writer, _) = listener.accept().unwrap();
        writer.write_all(&[1; 128]).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !owner.snapshot().under_pressure || !context.pending() {
            assert!(
                std::time::Instant::now() < deadline,
                "pressure delivery timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "UI callback must not run on watcher"
        );
        assert!(
            owner
                .reserve(MemoryDomain::Cpu, MemoryClass::Prefetch, 4)
                .is_none()
        );
        assert!(
            unaffected
                .reserve(MemoryDomain::Cpu, MemoryClass::Prefetch, 4)
                .is_some()
        );
        drop(watch);
        assert!(!context.pending(), "shutdown must remove pending delivery");
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        drop(writer);
        drop(listener);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
