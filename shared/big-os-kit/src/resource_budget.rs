// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! Admission before allocating retained payloads. This accounts for payload bytes,
//! not allocator overhead or GPU memory. A reservation belongs to one allocation;
//! put both inside the same Arc when consumers share the allocation.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Fixed process-wide payload budget, without another worker or daemon.
#[derive(Debug)]
pub struct Budget {
    limit: usize,
    used: AtomicUsize,
}
impl Budget {
    /// Create a budget suitable for a static shared owner.
    pub const fn new(limit: usize) -> Self {
        Self {
            limit,
            used: AtomicUsize::new(0),
        }
    }
    /// Reserve before allocating; overflow and exhaustion both refuse admission.
    #[must_use]
    pub fn reserve(&'static self, bytes: usize) -> Option<Reservation> {
        self.used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|next| *next <= self.limit)
            })
            .ok()?;
        Some(Reservation {
            budget: self,
            bytes,
        })
    }
    /// Logical payload bytes still owned, including allocations in flight.
    #[must_use]
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }
}

/// Non-cloneable admission token released with the allocation it accompanies.
#[derive(Debug)]
pub struct Reservation {
    budget: &'static Budget,
    bytes: usize,
}
impl Reservation {
    /// Bytes still reserved by this token.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Return unused admission after an operation discovers its actual size.
    /// Refuses growth without changing the reservation or the shared counter.
    /// This changes accounting, not the allocation itself.
    #[must_use]
    pub fn shrink_to(&mut self, bytes: usize) -> bool {
        if bytes > self.bytes {
            return false;
        }
        let released = self.bytes - bytes;
        self.bytes = bytes;
        self.budget.used.fetch_sub(released, Ordering::AcqRel);
        true
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admission_releases_once_refuses_overflow() {
        let budget: &'static Budget = Box::leak(Box::new(Budget::new(10)));
        let held = budget.reserve(7).expect("within budget");
        assert!(budget.reserve(4).is_none());
        assert!(budget.reserve(usize::MAX).is_none());
        assert_eq!(budget.used(), 7);
        drop(held);
        assert_eq!(budget.used(), 0);
        assert!(budget.reserve(10).is_some());
    }
    #[test]
    fn shrinking_returns_only_unused_bytes() {
        let budget: &'static Budget = Box::leak(Box::new(Budget::new(10)));
        let mut held = budget.reserve(10).expect("fits");
        assert!(held.shrink_to(4));
        assert_eq!(budget.used(), 4);
        assert!(!held.shrink_to(11));
        assert_eq!(budget.used(), 4);
        let other = budget.reserve(6).expect("returned space reusable");
        drop(held);
        assert_eq!(budget.used(), 6);
        drop(other);
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn shared_allocation_keeps_one_reservation() {
        let budget: &'static Budget = Box::leak(Box::new(Budget::new(8)));
        let allocation = std::sync::Arc::new((vec![0_u8; 8], budget.reserve(8).expect("fits")));
        let shared = allocation.clone();
        drop(allocation);
        assert_eq!(budget.used(), 8);
        drop(shared);
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn parallel_reservations_stay_within_budget() {
        let budget: &'static Budget = Box::leak(Box::new(Budget::new(1024)));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(move || {
                    for _ in 0..1000 {
                        if let Some(held) = budget.reserve(256) {
                            assert!(budget.used() <= 1024);
                            std::thread::yield_now();
                            drop(held);
                        }
                    }
                });
            }
        });
        assert_eq!(budget.used(), 0);
    }
}
