// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Cooperative admission for one Rust composition, with no pool or polling timer.
//!
//! The owner is explicit and cloneable; leases keep it alive until the last real
//! allocation or job is released. CPU payload and GPU *estimates* are separate
//! series and must not be added to PSS, especially on unified-memory GPUs.
//! This is not a hard RSS limit: decoder scratch, drivers and unadopted callers
//! can allocate elsewhere. Dynamically linked Rust copies do not share statics;
//! [`shared`] is shared only inside one linked composition (e.g. Workbench).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

const MIB: usize = 1024 * 1024;
const PRESSURE_QUIET_PERIOD: Duration = Duration::from_secs(5);

/// What the admission estimates. These are independent ledgers, not additive RAM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum MemoryDomain {
    /// Owned CPU pixel/payload capacity, excluding allocator and decoder overhead.
    Cpu = 0,
    /// Estimated GPU payload, excluding driver metadata and intermediate surfaces.
    GpuEstimate = 1,
}

/// Purpose of the live charge. Explicit promotion moves class counters together
/// under the owner lock, never dropping the total or double-charging pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum MemoryClass {
    /// Payload explicitly requested by the user, including a visible frame.
    Interactive = 0,
    /// Optional data which can be rebuilt; competes with prefetch for one quota.
    Reconstructible = 1,
    /// Speculative neighbors; refused during the pressure quiet period.
    Prefetch = 2,
    /// Temporary payload for an explicit export, never evicted behind the user.
    Export = 3,
}

impl MemoryClass {
    fn optional(self) -> bool {
        matches!(self, Self::Reconstructible | Self::Prefetch)
    }
}

/// Shared job admission. No permit implies defer, not a polling/spin loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkClass {
    /// Work needed for the current user interaction.
    Interactive,
    /// Optional/background work, limited separately to leave interactive capacity.
    Background,
}

/// Limits are payload allowances, not preallocations or promises of total RAM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceLimits {
    /// Independent total CPU and estimated GPU limits, indexed by MemoryDomain.
    pub bytes: [usize; 2],
    /// Combined Reconstructible + Prefetch ceiling inside each domain's total.
    pub optional_bytes: [usize; 2],
    /// Upper bound used by display-only image refinement, never export.
    pub display_frame_bytes: usize,
    /// Total admitted jobs, including background jobs.
    pub jobs: usize,
    /// Background subset of total jobs.
    pub background_jobs: usize,
}

impl ResourceLimits {
    /// Conservative initial profile for <= 6 GiB or unknown capacity. The caller
    /// may supply an effective cgroup limit rather than the machine's RAM.
    /// These are starting policies, not measured optimal values for all GPUs.
    #[must_use]
    pub fn for_capacity(memory_bytes: Option<u64>, logical_cpus: usize) -> Self {
        let small = memory_bytes.is_none_or(|bytes| bytes <= 6 * 1024 * 1024 * 1024);
        // Keep the 4 GiB reference profile, but do not allocate the same fixed
        // allowance in a 512 MiB service/container. This is an admission policy,
        // not a reservation of the entire cgroup or a total-process RSS cap.
        let profile_bytes = if small { 192 * MIB } else { 512 * MIB };
        let bytes = memory_bytes.map_or(profile_bytes, |capacity| {
            usize::try_from(capacity / 16)
                .unwrap_or(usize::MAX)
                .min(profile_bytes)
        });
        let optional = (bytes / 3).min(if small { 64 * MIB } else { 128 * MIB });
        Self {
            bytes: [bytes; 2],
            optional_bytes: [optional; 2],
            display_frame_bytes: (bytes / 2).min(if small { 64 * MIB } else { 256 * MIB }),
            jobs: logical_cpus.clamp(1, 4),
            background_jobs: if small || logical_cpus < 4 { 1 } else { 2 },
        }
    }
}

/// One coherent observation. No private paths or user content are collected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceSnapshot {
    /// Admission policy; optional quotas are subsets, not extra allocations.
    pub limits: ResourceLimits,
    /// Live bytes by domain and admission class.
    pub bytes: [[usize; 4]; 2],
    /// Total and background live jobs.
    pub jobs: usize,
    /// Background subset of jobs.
    pub background_jobs: usize,
    /// Whether optional admission is currently suppressed after pressure.
    pub under_pressure: bool,
    /// Saturating refusal counter, useful for on-demand diagnosis.
    pub refused: u64,
}

#[derive(Default)]
struct State {
    bytes: [[usize; 4]; 2],
    jobs: usize,
    background_jobs: usize,
    quiet_until: Option<Instant>,
    refused: u64,
}

/// Session/composition-owned accounting. Methods never call user code under lock.
pub struct SessionResources {
    limits: ResourceLimits,
    state: Mutex<State>,
}

impl SessionResources {
    /// Construct an independent, explicitly owned governor. Invalid subset
    /// limits are clamped; zero limits refuse positive admission.
    #[must_use]
    pub fn new(mut limits: ResourceLimits) -> Arc<Self> {
        for (optional, total) in limits.optional_bytes.iter_mut().zip(limits.bytes) {
            *optional = (*optional).min(total);
        }
        limits.background_jobs = limits.background_jobs.min(limits.jobs);
        limits.display_frame_bytes = limits.display_frame_bytes.min(limits.bytes[0]);
        Arc::new(Self {
            limits,
            state: Mutex::new(State::default()),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // No callbacks/allocations occur while locked. Recovering poisoning is
        // safe here: each update is a checked, fixed-size bookkeeping operation.
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Snapshot all counters under a single lock; this is logical accounting.
    #[must_use]
    pub fn snapshot(&self) -> ResourceSnapshot {
        self.snapshot_at(Instant::now())
    }

    fn snapshot_at(&self, now: Instant) -> ResourceSnapshot {
        let state = self.state();
        ResourceSnapshot {
            limits: self.limits,
            bytes: state.bytes,
            jobs: state.jobs,
            background_jobs: state.background_jobs,
            under_pressure: state.quiet_until.is_some_and(|until| now < until),
            refused: state.refused,
        }
    }

    /// Admission before allocating. The lease must travel with the unique
    /// storage, not with a widget or a clone of its reference.
    #[must_use]
    pub fn reserve(
        self: &Arc<Self>,
        domain: MemoryDomain,
        class: MemoryClass,
        bytes: usize,
    ) -> Option<MemoryLease> {
        self.reserve_at(domain, class, bytes, Instant::now())
    }

    fn reserve_at(
        self: &Arc<Self>,
        domain: MemoryDomain,
        class: MemoryClass,
        bytes: usize,
        now: Instant,
    ) -> Option<MemoryLease> {
        let mut state = self.state();
        let ledger = state.bytes[domain as usize];
        // The total invariant makes this sum safe; checked arithmetic also
        // refuses pathological caller input rather than wrapping admission.
        let total = ledger.iter().try_fold(0usize, |a, b| a.checked_add(*b));
        let optional = ledger[MemoryClass::Reconstructible as usize]
            .checked_add(ledger[MemoryClass::Prefetch as usize]);
        let quiet = state.quiet_until.is_some_and(|until| now < until);
        let fits = total
            .and_then(|used| used.checked_add(bytes))
            .is_some_and(|used| used <= self.limits.bytes[domain as usize])
            && (!class.optional()
                || (!quiet
                    && optional
                        .and_then(|used| used.checked_add(bytes))
                        .is_some_and(|used| used <= self.limits.optional_bytes[domain as usize])));
        if !fits {
            state.refused = state.refused.saturating_add(1);
            return None;
        }
        state.bytes[domain as usize][class as usize] += bytes;
        drop(state);
        Some(MemoryLease {
            owner: Arc::clone(self),
            domain,
            class: AtomicUsize::new(class as usize),
            bytes,
        })
    }

    /// Acquire one job without creating a thread, queue or timer. A background
    /// refusal is expected; callers keep their cheap fallback or retry on a new
    /// user event, never by spinning.
    #[must_use]
    pub fn try_work(self: &Arc<Self>, class: WorkClass) -> Option<WorkLease> {
        self.try_work_at(class, Instant::now())
    }

    fn try_work_at(self: &Arc<Self>, class: WorkClass, now: Instant) -> Option<WorkLease> {
        let mut state = self.state();
        let background = class == WorkClass::Background;
        if state.jobs >= self.limits.jobs
            || (background
                && (state.background_jobs >= self.limits.background_jobs
                    || state.quiet_until.is_some_and(|until| now < until)))
        {
            state.refused = state.refused.saturating_add(1);
            return None;
        }
        state.jobs += 1;
        state.background_jobs += usize::from(background);
        drop(state);
        Some(WorkLease {
            owner: Arc::clone(self),
            background,
        })
    }

    /// Suppress new optional work/payload for a short quiet period after pressure.
    /// Existing visible allocations are not invalidated or evicted. No timer is
    /// installed: admission lazily observes expiry, repeated events extend it.
    pub fn note_pressure(&self) {
        self.note_pressure_at(Instant::now());
    }

    fn note_pressure_at(&self, now: Instant) {
        self.state().quiet_until = now.checked_add(PRESSURE_QUIET_PERIOD);
    }
}

/// Non-cloneable lease tied to one allocation. Put it inside the shared owner.
pub struct MemoryLease {
    owner: Arc<SessionResources>,
    domain: MemoryDomain,
    class: AtomicUsize,
    bytes: usize,
}

impl std::fmt::Debug for MemoryLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryLease")
            .field("domain", &self.domain)
            .field("class", &self.class())
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl MemoryLease {
    /// Current class. Only typed enum values are ever written; all ledger moves
    /// and the final drop are serialized by the resource owner's mutex.
    #[must_use]
    pub fn class(&self) -> MemoryClass {
        const CLASSES: [MemoryClass; 4] = [
            MemoryClass::Interactive,
            MemoryClass::Reconstructible,
            MemoryClass::Prefetch,
            MemoryClass::Export,
        ];
        CLASSES[self.class.load(Ordering::Relaxed)]
    }

    /// Promote an optional allocation already displayed to Interactive without
    /// releasing its total charge. Multiple Arc consumers may promote at once;
    /// only the first moves the counters. Exports are not reclassified.
    pub fn promote_for_display(&self) {
        let mut state = self.owner.state();
        let class = self.class();
        if class.optional() {
            state.bytes[self.domain as usize][class as usize] -= self.bytes;
            state.bytes[self.domain as usize][MemoryClass::Interactive as usize] += self.bytes;
            self.class
                .store(MemoryClass::Interactive as usize, Ordering::Relaxed);
        }
    }

    /// Move a displayed allocation that left the screen to the optional
    /// ledger as reconstructible, keeping its total charge. No admission is
    /// asked: the optional ledger may go over its ceiling, and the owner then
    /// drops spare allocations until it fits. Other classes stay as they are.
    pub fn retire_from_display(&self) {
        let mut state = self.owner.state();
        if self.class() == MemoryClass::Interactive {
            state.bytes[self.domain as usize][MemoryClass::Interactive as usize] -= self.bytes;
            state.bytes[self.domain as usize][MemoryClass::Reconstructible as usize] += self.bytes;
            self.class
                .store(MemoryClass::Reconstructible as usize, Ordering::Relaxed);
        }
    }

    /// Capacity still charged.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Domain this token charges. CPU storage must not accept a GPU estimate.
    #[must_use]
    pub fn domain(&self) -> MemoryDomain {
        self.domain
    }

    /// Return overestimated capacity; never grow an existing reservation.
    #[must_use]
    pub fn shrink_to(&mut self, bytes: usize) -> bool {
        if bytes > self.bytes {
            return false;
        }
        let released = self.bytes - bytes;
        self.owner.state().bytes[self.domain as usize][self.class() as usize] -= released;
        self.bytes = bytes;
        true
    }
}

impl Drop for MemoryLease {
    fn drop(&mut self) {
        self.owner.state().bytes[self.domain as usize][self.class() as usize] -= self.bytes;
    }
}

/// A job keeps its permit through completion, not merely through cancellation.
pub struct WorkLease {
    owner: Arc<SessionResources>,
    background: bool,
}

impl Drop for WorkLease {
    fn drop(&mut self) {
        let mut state = self.owner.state();
        state.jobs -= 1;
        state.background_jobs -= usize::from(self.background);
    }
}

/// The default owner for constructors given none. One owner inside this process;
/// not a cross-cdylib or cross-process singleton. Callers may instead explicitly
/// pass an Arc created by [`SessionResources::new`] for isolated sessions/tests.
#[must_use]
pub fn shared() -> &'static Arc<SessionResources> {
    static OWNER: OnceLock<Arc<SessionResources>> = OnceLock::new();
    OWNER.get_or_init(|| {
        // Unknown capacity is conservative. This profile never scales up from
        // logical CPU count alone on a low-memory machine.
        let memory = crate::system_memory::effective_memory_bytes();
        let cpus = std::thread::available_parallelism().map_or(1, usize::from);
        SessionResources::new(ResourceLimits::for_capacity(memory, cpus))
    })
}

#[cfg(test)]
mod tests;
