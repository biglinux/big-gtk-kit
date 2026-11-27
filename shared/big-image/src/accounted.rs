// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Immutable CPU pixel storage with an optional payload admission lease.
//! GPU allocations, decoder scratch and allocator overhead are not included.

use std::ops::Deref;

use big_os_kit::session_resources::{MemoryDomain, MemoryLease};

use crate::{BigImageError, BigRgbaImage};

/// One pixel allocation and its admission, shared together inside an `Arc`.
///
/// Consumers cannot mutate or grow the admitted storage. Cache hits and GTK
/// byte owners clone the same `Arc`; neither promotes pixels out of accounting.
/// Existing foreground paths can use `From<BigRgbaImage>` without admission;
/// this wrapper alone is not a limit on all images or on total process memory.
#[derive(Debug)]
pub struct AccountedRgba {
    // Drop pixels before returning their admission to the budget.
    image: BigRgbaImage,
    _session: Option<MemoryLease>,
}

impl AccountedRgba {
    /// Mark an already admitted frame as visible. Its total charge stays live;
    /// only the optional-class subtotal changes.
    pub fn promote_for_display(&self) {
        if let Some(lease) = &self._session {
            lease.promote_for_display();
        }
    }

    /// Mark a frame that left the screen as rebuildable, so it counts against
    /// the optional allowance again. Its total charge stays live.
    pub fn retire_from_display(&self) {
        if let Some(lease) = &self._session {
            lease.retire_from_display();
        }
    }

    /// Attach a shared composition admission. No pixels are copied.
    ///
    /// # Errors
    /// Rejects non-CPU leases and capacity exceeding the admission. Refusal
    /// drops the pixels before returning the lease.
    pub fn with_session_admission(
        image: BigRgbaImage,
        mut session: MemoryLease,
    ) -> Result<Self, BigImageError> {
        if session.domain() != MemoryDomain::Cpu || image.rgba.capacity() > session.bytes() {
            drop(image);
            return Err(BigImageError::InvalidSource {
                reason: "pixel allocation exceeded CPU admission",
            });
        }
        let _ = session.shrink_to(image.rgba.capacity());
        Ok(Self {
            image,
            _session: Some(session),
        })
    }
}

impl From<BigRgbaImage> for AccountedRgba {
    fn from(image: BigRgbaImage) -> Self {
        Self {
            image,
            _session: None,
        }
    }
}

impl Deref for AccountedRgba {
    type Target = BigRgbaImage;

    fn deref(&self) -> &Self::Target {
        &self.image
    }
}

impl AsRef<BigRgbaImage> for AccountedRgba {
    fn as_ref(&self) -> &BigRgbaImage {
        &self.image
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn image() -> BigRgbaImage {
        BigRgbaImage {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 255],
            cicp: None,
            opaque: true,
        }
    }

    #[test]
    #[ignore = "display: needs a GTK display"]
    fn texture_owns_pixels_until_last_reference() {
        assert!(
            big_testkit::harness::run_gtk_test(|| {
                let owner = big_os_kit::session_resources::SessionResources::new(
                    big_os_kit::session_resources::ResourceLimits::for_capacity(None, 2),
                );
                let session = owner
                    .reserve(
                        MemoryDomain::Cpu,
                        big_os_kit::session_resources::MemoryClass::Interactive,
                        4,
                    )
                    .unwrap();
                let pixels =
                    Arc::new(AccountedRgba::with_session_admission(image(), session).unwrap());
                let texture = crate::rgba_to_shared_texture(&pixels).expect("valid pixels");
                let second_texture = texture.clone();
                drop(pixels);
                drop(texture);
                assert_eq!(owner.snapshot().bytes[0][0], 4);
                drop(second_texture);
                assert_eq!(owner.snapshot().bytes, [[0; 4]; 2]);
            }),
            "GTK lifetime test was not executed"
        );
    }

    #[test]
    fn unaccounted_owner_keeps_pixel_api() {
        let pixels = AccountedRgba::from(image());
        assert_eq!(pixels.as_ref(), &image());
        assert_eq!(pixels.width, 1);
    }

    fn session() -> std::sync::Arc<big_os_kit::session_resources::SessionResources> {
        use big_os_kit::session_resources::{ResourceLimits, SessionResources};
        SessionResources::new(ResourceLimits {
            bytes: [64, 64],
            optional_bytes: [32, 32],
            display_frame_bytes: 64,
            jobs: 1,
            background_jobs: 1,
        })
    }

    #[test]
    fn charges_follow_gbytes() {
        use big_os_kit::session_resources::MemoryClass;
        let r = session();
        let pixels = image();
        let ptr = pixels.rgba.as_ptr();
        let capacity = pixels.rgba.capacity();
        let pixels = Arc::new(
            AccountedRgba::with_session_admission(
                pixels,
                r.reserve(MemoryDomain::Cpu, MemoryClass::Prefetch, 32)
                    .unwrap(),
            )
            .unwrap(),
        );
        let bytes = gtk::glib::Bytes::from_owned(crate::SharedRgba(pixels.clone()));
        assert_eq!(bytes.as_ref().as_ptr(), ptr);
        drop(pixels);
        assert_eq!(r.snapshot().bytes[0][2], capacity);
        drop(bytes);
        assert_eq!(r.snapshot().bytes[0][2], 0);
    }

    #[test]
    fn mismatched_buffer_stays_accounted() {
        use big_os_kit::session_resources::MemoryClass;
        let r = session();
        assert!(
            AccountedRgba::with_session_admission(
                image(),
                r.reserve(MemoryDomain::GpuEstimate, MemoryClass::Interactive, 4)
                    .unwrap(),
            )
            .is_err()
        );
        assert!(
            AccountedRgba::with_session_admission(
                image(),
                r.reserve(MemoryDomain::Cpu, MemoryClass::Interactive, 3)
                    .unwrap(),
            )
            .is_err()
        );
        assert_eq!(r.snapshot().bytes, [[0; 4]; 2]);
    }
}
