// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

use super::*;

fn owner() -> Arc<SessionResources> {
    SessionResources::new(ResourceLimits {
        bytes: [100, 80],
        optional_bytes: [40, 30],
        display_frame_bytes: 64,
        jobs: 2,
        background_jobs: 1,
    })
}

#[test]
fn domains_are_independent_and_overflow_refused() {
    let r = owner();
    let cpu = r
        .reserve(MemoryDomain::Cpu, MemoryClass::Interactive, 100)
        .unwrap();
    let gpu = r
        .reserve(MemoryDomain::GpuEstimate, MemoryClass::Interactive, 80)
        .unwrap();
    assert!(
        r.reserve(MemoryDomain::Cpu, MemoryClass::Interactive, 1)
            .is_none()
    );
    assert!(
        r.reserve(MemoryDomain::GpuEstimate, MemoryClass::Export, usize::MAX)
            .is_none()
    );
    assert_eq!(r.snapshot().bytes, [[100, 0, 0, 0], [80, 0, 0, 0]]);
    drop((cpu, gpu));
    assert_eq!(r.snapshot().bytes, [[0; 4]; 2]);
}

#[test]
fn optional_classes_share_one_subset() {
    let r = owner();
    let cache = r
        .reserve(MemoryDomain::Cpu, MemoryClass::Reconstructible, 25)
        .unwrap();
    let speculative = r
        .reserve(MemoryDomain::Cpu, MemoryClass::Prefetch, 15)
        .unwrap();
    assert!(
        r.reserve(MemoryDomain::Cpu, MemoryClass::Prefetch, 1)
            .is_none()
    );
    let visible = r
        .reserve(MemoryDomain::Cpu, MemoryClass::Interactive, 60)
        .unwrap();
    assert!(
        r.reserve(MemoryDomain::Cpu, MemoryClass::Export, 1)
            .is_none()
    );
    drop((cache, speculative, visible));
    assert_eq!(r.snapshot().bytes, [[0; 4]; 2]);
}

#[test]
fn shrink_releases_capacity_once() {
    let r = owner();
    let mut lease = r
        .reserve(MemoryDomain::Cpu, MemoryClass::Export, 80)
        .unwrap();
    assert!(lease.shrink_to(30));
    assert!(!lease.shrink_to(31));
    assert_eq!(lease.bytes(), 30);
    assert_eq!(r.snapshot().bytes[0][3], 30);
    assert!(lease.shrink_to(0));
    drop(lease);
    assert_eq!(r.snapshot().bytes[0][3], 0);
}

#[test]
fn last_consumer_owns_the_charge() {
    let r = owner();
    let weak = Arc::downgrade(&r);
    let allocation = Arc::new((
        vec![0u8; 8],
        r.reserve(MemoryDomain::Cpu, MemoryClass::Interactive, 8)
            .unwrap(),
    ));
    let consumer = Arc::clone(&allocation);
    drop(allocation);
    assert_eq!(r.snapshot().bytes[0][0], 8);
    drop(r);
    assert!(weak.upgrade().is_some());
    drop(consumer);
    assert!(weak.upgrade().is_none());
}

#[test]
fn pressure_blocks_only_optional_admission() {
    let r = owner();
    let now = Instant::now();
    let old = r
        .reserve(MemoryDomain::Cpu, MemoryClass::Prefetch, 10)
        .unwrap();
    r.note_pressure_at(now);
    assert!(
        r.reserve_at(MemoryDomain::Cpu, MemoryClass::Prefetch, 1, now)
            .is_none()
    );
    assert!(
        r.reserve_at(MemoryDomain::Cpu, MemoryClass::Reconstructible, 1, now)
            .is_none()
    );
    assert!(
        r.reserve_at(MemoryDomain::Cpu, MemoryClass::Interactive, 50, now)
            .is_some()
    );
    assert_eq!(r.snapshot().bytes[0][2], 10);
    assert!(r.try_work_at(WorkClass::Background, now).is_none());
    assert!(r.try_work_at(WorkClass::Interactive, now).is_some());
    let resumed = now + PRESSURE_QUIET_PERIOD;
    assert!(!r.snapshot_at(resumed).under_pressure);
    assert!(
        r.reserve_at(MemoryDomain::Cpu, MemoryClass::Prefetch, 1, resumed)
            .is_some()
    );
    assert!(r.try_work_at(WorkClass::Background, resumed).is_some());
    drop(old);
}

#[test]
fn pressure_bursts_extend_quiet_period() {
    let r = owner();
    let now = Instant::now();
    r.note_pressure_at(now);
    r.note_pressure_at(now + Duration::from_secs(3));
    assert!(r.snapshot_at(now + Duration::from_secs(6)).under_pressure);
    assert!(!r.snapshot_at(now + Duration::from_secs(8)).under_pressure);
}

/// An event sampled earlier but recorded later, by a caller paused between
/// the two, does not cut short the quiet a later event began.
#[test]
fn late_older_pressure_keeps_quiet_period() {
    let r = owner();
    let now = Instant::now();
    r.note_pressure_at(now + Duration::from_secs(3));
    r.note_pressure_at(now);
    assert!(r.snapshot_at(now + Duration::from_secs(6)).under_pressure);
}

#[test]
fn background_work_leaves_interactive_capacity() {
    let r = owner();
    let bg = r.try_work(WorkClass::Background).unwrap();
    assert!(r.try_work(WorkClass::Background).is_none());
    let fg = r.try_work(WorkClass::Interactive).unwrap();
    assert!(r.try_work(WorkClass::Interactive).is_none());
    assert_eq!((r.snapshot().jobs, r.snapshot().background_jobs), (2, 1));
    drop(bg);
    assert_eq!((r.snapshot().jobs, r.snapshot().background_jobs), (1, 0));
    drop(fg);
    assert_eq!(r.snapshot().jobs, 0);
}

#[test]
fn worker_thread_returns_its_permit() {
    let r = owner();
    let permit = r.try_work(WorkClass::Background).unwrap();
    std::thread::spawn(move || drop(permit)).join().unwrap();
    assert_eq!(r.snapshot().jobs, 0);
}

#[test]
fn admission_coherent_under_contention() {
    let r = owner();
    std::thread::scope(|scope| {
        for i in 0..8 {
            let r = Arc::clone(&r);
            scope.spawn(move || {
                for _ in 0..1000 {
                    let class = if i % 2 == 0 {
                        MemoryClass::Interactive
                    } else {
                        MemoryClass::Prefetch
                    };
                    if let Some(lease) = r.reserve(MemoryDomain::Cpu, class, 10) {
                        let s = r.snapshot();
                        assert!(s.bytes[0].iter().sum::<usize>() <= 100);
                        assert!(s.bytes[0][1] + s.bytes[0][2] <= 40);
                        std::thread::yield_now();
                        drop(lease);
                    }
                }
            });
        }
    });
    assert_eq!(r.snapshot().bytes, [[0; 4]; 2]);
}

#[test]
fn independent_owners_have_separate_limits() {
    let a = owner();
    let b = owner();
    let held = a
        .reserve(MemoryDomain::Cpu, MemoryClass::Interactive, 100)
        .unwrap();
    assert_eq!(b.snapshot().bytes, [[0; 4]; 2]);
    assert!(
        b.reserve(MemoryDomain::Cpu, MemoryClass::Interactive, 100)
            .is_some()
    );
    drop(held);
}

#[test]
fn invalid_subset_limits_are_deterministic() {
    let r = SessionResources::new(ResourceLimits {
        bytes: [0, 0],
        optional_bytes: [100, 100],
        display_frame_bytes: 100,
        jobs: 0,
        background_jobs: 100,
    });
    assert_eq!(r.snapshot().limits.optional_bytes, [0, 0]);
    assert_eq!(r.snapshot().limits.background_jobs, 0);
    assert!(
        r.reserve(MemoryDomain::Cpu, MemoryClass::Interactive, 0)
            .is_some()
    );
    assert!(
        r.reserve(MemoryDomain::Cpu, MemoryClass::Interactive, 1)
            .is_none()
    );
    assert!(r.try_work(WorkClass::Interactive).is_none());
}

#[test]
fn small_machines_get_small_defaults() {
    let small = ResourceLimits::for_capacity(Some(4 * 1024 * 1024 * 1024), 4);
    let unknown = ResourceLimits::for_capacity(None, 128);
    assert_eq!(small.bytes, unknown.bytes);
    assert_eq!(small.background_jobs, 1);
    assert_eq!(
        ResourceLimits::for_capacity(Some(4 * 1024 * 1024 * 1024), 1).jobs,
        1
    );
    assert!(
        ResourceLimits::for_capacity(Some(16 * 1024 * 1024 * 1024), 8).bytes[0] > small.bytes[0]
    );
}

#[test]
fn display_promotion_frees_optional_quota() {
    let owner = SessionResources::new(ResourceLimits {
        bytes: [12, 12],
        optional_bytes: [4, 4],
        display_frame_bytes: 12,
        jobs: 1,
        background_jobs: 1,
    });
    let lease = owner
        .reserve(MemoryDomain::Cpu, MemoryClass::Prefetch, 4)
        .unwrap();
    assert!(
        owner
            .reserve(MemoryDomain::Cpu, MemoryClass::Reconstructible, 1)
            .is_none()
    );
    lease.promote_for_display();
    assert_eq!(lease.class(), MemoryClass::Interactive);
    assert_eq!(owner.snapshot().bytes[0], [4, 0, 0, 0]);
    let optional = owner
        .reserve(MemoryDomain::Cpu, MemoryClass::Prefetch, 4)
        .unwrap();
    assert_eq!(owner.snapshot().bytes[0], [4, 0, 4, 0]);
    assert!(
        owner
            .reserve(MemoryDomain::Cpu, MemoryClass::Interactive, 5)
            .is_none()
    );
    drop(lease);
    drop(optional);
    assert_eq!(owner.snapshot().bytes, [[0; 4]; 2]);
}

#[test]
fn retired_frame_counts_as_optional() {
    let owner = SessionResources::new(ResourceLimits {
        bytes: [12, 12],
        optional_bytes: [4, 4],
        display_frame_bytes: 12,
        jobs: 1,
        background_jobs: 1,
    });
    let shown = owner
        .reserve(MemoryDomain::Cpu, MemoryClass::Interactive, 8)
        .unwrap();
    shown.retire_from_display();
    assert_eq!(shown.class(), MemoryClass::Reconstructible);
    assert_eq!(owner.snapshot().bytes[0], [0, 8, 0, 0]);
    assert!(
        owner
            .reserve(MemoryDomain::Cpu, MemoryClass::Prefetch, 1)
            .is_none()
    );
    shown.promote_for_display();
    assert_eq!(owner.snapshot().bytes[0], [8, 0, 0, 0]);
    let export = owner
        .reserve(MemoryDomain::Cpu, MemoryClass::Export, 4)
        .unwrap();
    export.retire_from_display();
    assert_eq!(export.class(), MemoryClass::Export);
    drop(shown);
    drop(export);
    assert_eq!(owner.snapshot().bytes, [[0; 4]; 2]);
}

#[test]
fn concurrent_promotion_moves_charge_once() {
    let owner = SessionResources::new(ResourceLimits::for_capacity(None, 2));
    let lease = Arc::new(
        owner
            .reserve(MemoryDomain::Cpu, MemoryClass::Prefetch, 64)
            .unwrap(),
    );
    let mut threads = Vec::new();
    for _ in 0..8 {
        let lease = lease.clone();
        threads.push(std::thread::spawn(move || {
            for _ in 0..1000 {
                lease.promote_for_display();
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(owner.snapshot().bytes[0], [64, 0, 0, 0]);
    drop(lease);
    assert_eq!(owner.snapshot().bytes, [[0; 4]; 2]);
}

#[test]
fn small_cgroup_scales_payloads_down() {
    let limits = ResourceLimits::for_capacity(Some(512 << 20), 8);
    assert_eq!(limits.bytes, [32 * MIB; 2]);
    assert_eq!(limits.display_frame_bytes, 16 * MIB);
    assert!(limits.optional_bytes[0] <= limits.bytes[0] / 3);
    assert_eq!(limits.background_jobs, 1);
}

#[test]
fn zero_capacity_refuses_payload() {
    let zero = SessionResources::new(ResourceLimits::for_capacity(Some(0), 2));
    assert!(
        zero.reserve(MemoryDomain::Cpu, MemoryClass::Interactive, 1)
            .is_none()
    );
    assert_eq!(zero.snapshot().limits.bytes, [0; 2]);
    assert_eq!(ResourceLimits::for_capacity(None, 2).bytes, [192 * MIB; 2]);
}

#[test]
fn export_cannot_become_display_cache() {
    let owner = SessionResources::new(ResourceLimits::for_capacity(None, 2));
    let lease = owner
        .reserve(MemoryDomain::Cpu, MemoryClass::Export, 64)
        .unwrap();
    lease.promote_for_display();
    assert_eq!(lease.class(), MemoryClass::Export);
    assert_eq!(owner.snapshot().bytes[0], [0, 0, 0, 64]);
    drop(lease);
    assert_eq!(owner.snapshot().bytes, [[0; 4]; 2]);
}
