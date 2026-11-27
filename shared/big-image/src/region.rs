// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! A persistent decoder for deep zoom into a very large JPEG: the source is read
//! once and each region is a `clip` from what the sandbox already holds. Keep it
//! only while zoomed in; dropping it returns the process to glycin's pool.

use std::path::{Path, PathBuf};

use super::{BigImageError, BigRgbaImage};

/// A live sandboxed decoder that crops regions of one large image on demand.
pub struct BigRegionDecoder {
    inner: super::secure::SandboxedRegionDecoder,
}

impl BigRegionDecoder {
    /// The source's natural (full) pixel size, to map the viewport onto it.
    #[must_use]
    pub fn natural_size(&self) -> (u32, u32) {
        self.inner.natural()
    }

    /// Decode the source rectangle `clip` (natural pixels) at output `target`.
    ///
    /// # Errors
    ///
    /// Fails when the sandboxed decoder cannot deliver the region.
    pub async fn decode_region(
        &self,
        clip: (u32, u32, u32, u32),
        target: (i32, i32),
    ) -> Result<BigRgbaImage, BigImageError> {
        self.inner.decode_region(clip, target).await
    }
}

/// Whether `path` can be decoded by region: a JPEG with the reduce-decoding
/// loader active, since only it honors a clip.
///
/// The viewer uses this to choose native-detail tiles over a heavy full-image
/// re-decode when zooming a large JPEG, without first reading the file.
#[must_use]
pub fn region_supported(path: &Path) -> bool {
    region_capable(path)
}

fn region_capable(path: &Path) -> bool {
    super::large_jpeg_source_enabled()
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(extension.to_ascii_lowercase().as_str(), "jpg" | "jpeg")
            })
}

/// Open a persistent region decoder for a large JPEG.
///
/// `None` when region decode does not apply (not a JPEG, or the reduce-decoding
/// loader is not installed), so the caller keeps the base frame instead.
///
/// # Errors
///
/// Fails when the source cannot be staged or the sandbox is unavailable.
pub async fn open_region_decoder(
    path: impl Into<PathBuf>,
) -> Result<Option<BigRegionDecoder>, BigImageError> {
    let path = path.into();
    if !region_capable(&path) {
        return Ok(None);
    }
    crate::glycin_threads();
    // The bound is unused by the region decoder (each crop carries its own
    // target); stage through the shared path with a placeholder.
    let spec = super::BigImageSpec::bounded(path, 1, 1);
    let staged = gtk::gio::spawn_blocking(move || super::secure::stage_region(&spec))
        .await
        .map_err(|_| BigImageError::WorkerGone)??;
    let opened = super::secure::SandboxedRegionDecoder::open(staged).await?;
    Ok(opened.map(|inner| BigRegionDecoder { inner }))
}
