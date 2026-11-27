// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! Display-only admission; export/save must never reuse a reduced preview.

use crate::BigImageError;
use big_os_kit::session_resources::{MemoryClass, MemoryDomain, MemoryLease, SessionResources};
use std::sync::Arc;

/// Packed RGBA byte count for positive dimensions, refusing arithmetic overflow.
#[must_use]
pub fn rgba_capacity(bound: (i32, i32)) -> Option<usize> {
    if bound.0 <= 0 || bound.1 <= 0 {
        return None;
    }
    usize::try_from(bound.0)
        .ok()?
        .checked_mul(usize::try_from(bound.1).ok()?)?
        .checked_mul(4)
}

/// Largest aspect-preserving bound that fits the payload allowance. Integer
/// arithmetic avoids a rounded-up pixel exceeding the allowance on large inputs.
#[must_use]
pub fn bounded_rgba(bound: (i32, i32), bytes: usize) -> Option<(i32, i32)> {
    if bound.0 <= 0 || bound.1 <= 0 || bytes < 4 {
        return None;
    }
    let longest = bound.0.max(bound.1) as u64;
    let at = |edge: u64| {
        (
            (bound.0 as u64 * edge / longest).max(1) as i32,
            (bound.1 as u64 * edge / longest).max(1) as i32,
        )
    };
    let mut low = 1_u64;
    let mut high = longest;
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if rgba_capacity(at(mid)).is_some_and(|size| size <= bytes) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    Some(at(low))
}

/// Reserve a display frame before decode. Interactive previews may fall back to
/// a smaller bound; speculative requests must fit at their requested size.
/// No original/export pixels are resized by this function.
///
/// # Errors
/// Refuses invalid bounds or resource pressure/exhaustion. Interactive fallback
/// keeps at least 256 pixels on the long edge (or the original smaller bound),
/// rather than silently turning a photograph into a one-pixel placeholder.
pub fn admit_display(
    resources: &Arc<SessionResources>,
    requested: (i32, i32),
    class: MemoryClass,
) -> Result<((i32, i32), MemoryLease), BigImageError> {
    // Saving/exporting must request the exact allocation through the governor,
    // never silently use a presentation-quality fallback.
    if class == MemoryClass::Export {
        return Err(BigImageError::ResourceLimit);
    }
    let size = rgba_capacity(requested).ok_or(BigImageError::InvalidDisplayBound)?;
    if matches!(class, MemoryClass::Reconstructible | MemoryClass::Prefetch) {
        return resources
            .reserve(MemoryDomain::Cpu, class, size)
            .map(|lease| (requested, lease))
            .ok_or(BigImageError::ResourceLimit);
    }
    let mut bound = bounded_rgba(requested, resources.snapshot().limits.display_frame_bytes)
        .ok_or(BigImageError::ResourceLimit)?;
    let minimum = requested.0.max(requested.1).min(256);
    loop {
        if bound.0.max(bound.1) < minimum {
            return Err(BigImageError::ResourceLimit);
        }
        let bytes = rgba_capacity(bound).ok_or(BigImageError::InvalidDisplayBound)?;
        if let Some(lease) = resources.reserve(MemoryDomain::Cpu, class, bytes) {
            return Ok((bound, lease));
        }
        // A new owner may consume bytes between observation and reservation.
        // Halving the payload bounds retries to log2(bytes), without blocking.
        bound = bounded_rgba(bound, bytes / 2).ok_or(BigImageError::ResourceLimit)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use big_os_kit::session_resources::ResourceLimits;
    fn owner(bytes: usize) -> Arc<SessionResources> {
        SessionResources::new(ResourceLimits {
            bytes: [bytes, bytes],
            optional_bytes: [bytes / 2, bytes / 2],
            display_frame_bytes: bytes,
            jobs: 1,
            background_jobs: 1,
        })
    }
    #[test]
    fn integer_scaling_never_overadmits() {
        for bound in [
            (7680, 7680),
            (7680, 2160),
            (1, i32::MAX),
            (i32::MAX, 1),
            (i32::MAX, i32::MAX),
        ] {
            for bytes in [4, 7, 65536, 64 * 1024 * 1024] {
                let b = bounded_rgba(bound, bytes).unwrap();
                assert!(b.0 > 0 && b.1 > 0 && b.0 <= bound.0 && b.1 <= bound.1);
                assert!(rgba_capacity(b).unwrap() <= bytes);
            }
        }
        assert_eq!(
            bounded_rgba((7680, 4320), 64 * 1024 * 1024).unwrap(),
            (5461, 3071)
        );
    }
    #[test]
    fn display_admission_never_downsamples_export_requests() {
        assert!(
            admit_display(&owner(64 * 1024 * 1024), (7680, 7680), MemoryClass::Export).is_err()
        );
    }
    #[test]
    fn invalid_or_oversized_is_refused() {
        assert_eq!(rgba_capacity((0, 8)), None);
        assert_eq!(rgba_capacity((-1, 8)), None);
        assert_eq!(bounded_rgba((8, 8), 3), None);
        assert!(admit_display(&owner(3), (8, 8), MemoryClass::Interactive).is_err());
    }
    #[test]
    fn interactive_fallback_has_quality_floor() {
        let r = owner(256 * 256 * 4);
        let (bound, lease) = admit_display(&r, (4096, 4096), MemoryClass::Interactive).unwrap();
        assert_eq!(bound, (256, 256));
        assert_eq!(r.snapshot().bytes[0][0], 256 * 256 * 4);
        assert!(admit_display(&r, (4096, 4096), MemoryClass::Interactive).is_err());
        drop(lease);
        assert!(admit_display(&r, (4096, 4096), MemoryClass::Interactive).is_ok());
    }
    #[test]
    fn optional_refusal_keeps_capacity() {
        let r = owner(128 * 128 * 4);
        assert!(admit_display(&r, (256, 256), MemoryClass::Prefetch).is_err());
        assert_eq!(r.snapshot().bytes, [[0; 4]; 2]);
        let (bound, lease) = admit_display(&r, (8, 8), MemoryClass::Prefetch).unwrap();
        assert_eq!(bound, (8, 8));
        drop(lease);
    }
    #[test]
    fn pressure_spares_requested_preview() {
        let r = owner(1024 * 1024);
        r.note_pressure();
        assert!(admit_display(&r, (8, 8), MemoryClass::Prefetch).is_err());
        assert!(admit_display(&r, (8, 8), MemoryClass::Interactive).is_ok());
    }
}
