// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! One resource owner and pressure subscription for one linked UI composition.
//!
//! Acquire it on the application's owner thread before constructing panes.
//! Windows hold an `Rc` and the registry is weak; the last owner stops the
//! pressure subscription.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use big_os_kit::session_resources::{ResourceSnapshot, SessionResources};

use crate::memory_pressure::{self, MemoryPressureWatch};

thread_local! {
    static RUNTIME: RefCell<Weak<SessionRuntime>> = const { RefCell::new(Weak::new()) };
}

/// Owner-thread lifetime of the shared resources and pressure subscription.
pub struct SessionRuntime {
    resources: Arc<SessionResources>,
    // Held for its Drop: the subscription lives exactly as long as the runtime.
    _pressure: Option<MemoryPressureWatch>,
}

impl SessionRuntime {
    /// Join the existing runtime or create it without activating any product.
    /// If pressure monitoring is unavailable the resource limits still apply.
    #[must_use]
    pub fn acquire() -> Rc<Self> {
        acquire_with(&RUNTIME, || {
            let resources = big_os_kit::session_resources::shared().clone();
            let pressure = match memory_pressure::watch_scoped_for(
                resources.clone(),
                memory_pressure::release_registered,
            ) {
                Ok(watch) => watch,
                Err(error) => {
                    log::debug!("session memory pressure unavailable: {error}");
                    None
                }
            };
            Self {
                resources,
                _pressure: pressure,
            }
        })
    }

    /// Same cooperative owner used by legacy default pane constructors in this
    /// composition; Arc clones are not independent memory budgets.
    #[must_use]
    pub fn resources(&self) -> &Arc<SessionResources> {
        &self.resources
    }

    /// On-demand logical counters, not a claim about RSS/VRAM or decoder scratch.
    #[must_use]
    pub fn snapshot(&self) -> ResourceSnapshot {
        self.resources.snapshot()
    }
}

fn acquire_with(
    slot: &'static std::thread::LocalKey<RefCell<Weak<SessionRuntime>>>,
    create: impl FnOnce() -> SessionRuntime,
) -> Rc<SessionRuntime> {
    if let Some(runtime) = slot.with_borrow(Weak::upgrade) {
        return runtime;
    }
    // No caller code is run under a RefCell borrow. This initializer does not
    // call a product or iterate the GTK context.
    let runtime = Rc::new(create());
    slot.with_borrow_mut(|weak| *weak = Rc::downgrade(&runtime));
    runtime
}

#[cfg(test)]
mod tests {
    use super::*;
    use big_os_kit::session_resources::{MemoryClass, MemoryDomain, ResourceLimits};
    thread_local! {
        static TEST_RUNTIME: RefCell<Weak<SessionRuntime>> = const { RefCell::new(Weak::new()) };
    }
    fn runtime() -> Rc<SessionRuntime> {
        acquire_with(&TEST_RUNTIME, || SessionRuntime {
            resources: SessionResources::new(ResourceLimits::for_capacity(None, 2)),
            _pressure: None,
        })
    }
    #[test]
    fn users_share_one_owner() {
        let first = runtime();
        let second = runtime();
        assert!(Rc::ptr_eq(&first, &second));
        assert!(Arc::ptr_eq(first.resources(), second.resources()));
        let allocation = first
            .resources()
            .reserve(MemoryDomain::Cpu, MemoryClass::Interactive, 123)
            .unwrap();
        assert_eq!(second.snapshot().bytes[0][0], 123);
        drop(allocation);
        assert_eq!(second.snapshot().bytes[0][0], 0);
    }
    #[test]
    fn later_users_skip_initialization() {
        let first = runtime();
        let second = acquire_with(&TEST_RUNTIME, || panic!("must reuse"));
        drop(first);
        assert!(Rc::strong_count(&second) == 1);
    }
}
