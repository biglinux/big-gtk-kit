// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The image-loading substrate of the suite. Decoding goes through the system's
//! glycin sandbox (bubblewrap, or where user namespaces are off a loader that
//! confines itself with seccomp and Landlock) and fails closed when neither
//! can run, at the size the image is shown (an SVG is rendered at that size, a
//! raster never upscaled), on a worker: only the final `MemoryTexture` wrap
//! runs on the caller's thread.

use std::path::PathBuf;

use gtk::{gdk, gio, glib, prelude::*};

mod accounted;
pub mod admission;
mod animation;
mod crop;
pub mod exif;
pub mod gl_texture;
pub mod icon;
pub mod jpeg_loader;
mod jpeg_turn;
pub mod office_page;
mod region;
mod resize;
mod rotation;
pub(crate) mod save;
mod save_animation;
mod secure;
pub mod thumbnailer;
pub mod video_thumbnail;

pub use accounted::AccountedRgba;
pub use animation::{
    BigAnimation, BigAnimationFrame, displayed_delay, may_be_animated, open_animation,
};
pub use crop::{BigCropRect, crop_rgba};
pub use jpeg_turn::turned_jpeg;
pub use region::{BigRegionDecoder, open_region_decoder, region_supported};
pub use resize::{fit_within, resize_rgba, shrink_rgb16};
pub use rotation::{BigImageQuarterTurn, rotate_rgba_quarter_turn};
pub use save::{
    BIG_IMAGE_SAVE_FORMATS, BigCarriedMetadata, BigImageSaveFormat, BigSaveTurn, encode_rgba,
    png_bytes, save_pixel_ceiling, save_rgba, save_rgba_carrying,
};
pub use save_animation::{BigAnimationFormat, BigAnimationRefusal, BigAnimationWriter};

/// Decoded image as tightly packed RGBA8 plus dimensions. `Send`: decode on
/// a worker, wrap into a texture on the GTK thread ([`rgba_to_texture`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigRgbaImage {
    /// Tightly packed RGBA8, `width * height * 4` bytes, no row padding.
    pub rgba: Vec<u8>,
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
    /// The color space these pixels are in, as CICP code points, when the
    /// decoder said it was not plain sRGB.
    ///
    /// Without it the pixels go to GDK as if they were sRGB, so a Display-P3
    /// photograph or an HDR frame is drawn with the wrong primaries and the
    /// wrong transfer curve: visibly flat or oversaturated, silently.
    pub cicp: Option<BigCicp>,
    /// The source had no alpha channel, so the alpha bytes are all `255` and
    /// carry no information.
    ///
    /// An opaque image is wrapped as `R8g8b8x8` instead of `R8g8b8a8`: GDK then
    /// skips the straight-alpha premultiply pass on upload, KWin copies instead
    /// of blends, and the texture becomes eligible for `GtkGraphicsOffload`
    /// compositor passthrough. Transforms that preserve the pixels
    /// (rotate/crop/resize) preserve this flag.
    pub opaque: bool,
}

impl AsRef<BigRgbaImage> for BigRgbaImage {
    fn as_ref(&self) -> &BigRgbaImage {
        self
    }
}

/// What the decoder read about the image a frame was decoded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BigSourceSize {
    /// The image's own pixel size, turned to its EXIF orientation: what a
    /// full-resolution frame of it measures.
    pub natural: (i32, i32),
    /// Whether it is a vector, which renders sharp at any size.
    pub scalable: bool,
    /// Whether its colors are described in a space other than sRGB, by an
    /// ICC profile the decoder converts the frame from or by CICP code points:
    /// a copy is written in sRGB and leaves that description out.
    pub color_profile: bool,
}

/// Color space as the four CICP code points, which is what both Glycin
/// reports and GDK accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BigCicp {
    /// Color primaries code point.
    pub color_primaries: u32,
    /// Transfer characteristics code point.
    pub transfer_characteristics: u32,
    /// Matrix coefficients code point.
    pub matrix_coefficients: u32,
    /// Whether the range is full rather than narrow.
    pub full_range: bool,
}

/// Why a load failed: typed, so a failure is never a silent empty image.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BigImageError {
    /// Shared display resources are temporarily exhausted; the original is safe.
    #[error("display resources are busy; close unused previews and retry")]
    ResourceLimit,
    /// The file could not be read or decoded by any system loader.
    #[error("decode failed for {path}: {reason}")]
    Decode {
        /// Offending file.
        path: PathBuf,
        /// Loader error text.
        reason: String,
    },
    /// The decoded pixel buffer was malformed (zero-sized, overflow).
    #[error("invalid pixel data for {path}")]
    InvalidPixels {
        /// Offending file.
        path: PathBuf,
    },
    /// Decoded in-memory pixel data was malformed.
    #[error("invalid pixel data for in-memory image")]
    InvalidBytePixels,
    /// A sandbox-required request did not name a local readable regular file.
    #[error("image source is not a readable local regular file: {reason}")]
    InvalidSource {
        /// Path-independent reason safe to show to the user.
        reason: &'static str,
    },
    /// A sandbox-required request exceeded the accepted source-file size.
    #[error("image source exceeds the {max_bytes}-byte safety limit")]
    SourceTooLarge {
        /// Maximum accepted source size.
        max_bytes: u64,
    },
    /// The source or delivered frame exceeded the pixel safety limit.
    #[error("image dimensions {width}×{height} exceed the {max_pixels}-pixel safety limit")]
    PixelLimitExceeded {
        /// Reported width.
        width: u32,
        /// Reported height.
        height: u32,
        /// Maximum accepted pixel count.
        max_pixels: u64,
    },
    /// A sandbox-required request did not provide a usable display bound.
    #[error("sandboxed image decoding requires a positive bounded display size")]
    InvalidDisplayBound,
    /// No supported sandbox mechanism was available.
    #[error("sandboxed image decoding is unavailable")]
    SandboxUnavailable,
    /// Glycin could not decode the source inside the required sandbox.
    #[error("the sandboxed image decoder could not read this image")]
    SandboxedDecodeFailed,
    /// The chosen file name asks for a format this build cannot write.
    #[error("this image format cannot be written")]
    UnsupportedSaveFormat,
    /// The encoded image could not be written.
    #[error("the image could not be written: {reason}")]
    SaveFailed {
        /// Encoder or filesystem error text.
        reason: String,
    },
    /// The animation is past what this build writes; a caller shows
    /// [`BigAnimationRefusal::words`].
    #[error("{}", .0.words(str::to_owned))]
    AnimationRefused(BigAnimationRefusal),
    /// The animation has no frame after the last one.
    #[error("the animation has no more frames")]
    AnimationEnded,
    /// A caller cancelled an in-flight decode whose result it does not need.
    #[error("image decode was cancelled")]
    Cancelled,
    /// The worker thread died before answering.
    #[error("image decode worker vanished")]
    WorkerGone,
}

/// How a bounded texture should meet its destination.
///
/// This mirrors the four useful GTK `ContentFit` behaviors without making
/// this foundation crate depend on a widget. The decoder uses it only to pick
/// a memory-efficient pixel size; the final widget still owns presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BigImageFit {
    /// Preserve aspect ratio and fit entirely inside the destination.
    Contain,
    /// Preserve aspect ratio and cover the destination, cropping the overflow.
    Cover,
    /// Decode toward the destination dimensions even when that changes aspect ratio.
    Fill,
    /// Like `Contain`, but never decode larger than the source raster.
    ScaleDown,
}

/// One load request: a file plus the pixel bound it is decoded within.
#[derive(Debug, Clone)]
pub struct BigImageSpec {
    /// Source file.
    pub path: PathBuf,
    /// Bounding box in device pixels (aspect preserved). Every decode refuses
    /// `None`. Multiply logical size by the widget's `scale_factor()` for
    /// crisp HiDPI output.
    pub bound: Option<(i32, i32)>,
    /// Source sub-rectangle to decode, in natural pixels `(x, y, w, h)`. `None`
    /// decodes the whole image. Only the reduce-decoding JPEG loader honors it
    /// (deep zoom into a gigapixel), so it is set solely on that path; other
    /// loaders would return the whole image scaled and mislabel it as the region.
    pub clip: Option<(u32, u32, u32, u32)>,
    /// Decoded for a thumbnail, in processes at nice 10: a folder of pictures
    /// then waits for the threads of the program showing it, frames first.
    pub thumbnail: bool,
}

/// The nice value of the processes that draw thumbnails. They share the
/// cgroup of the program that shows them, where the kernel shares the CPU by
/// nice value: at 10 a busy decoder gets about a tenth of what the program's
/// own threads get, and all of what they leave.
pub(crate) const THUMBNAIL_NICE: i32 = 10;

impl BigImageSpec {
    /// Load `path` bounded by `width`×`height` device pixels.
    #[must_use]
    pub fn bounded(path: impl Into<PathBuf>, width: i32, height: i32) -> Self {
        Self {
            path: path.into(),
            bound: Some((width, height)),
            clip: None,
            thumbnail: false,
        }
    }

    /// A thumbnail of `path`, bounded like [`Self::bounded`].
    #[must_use]
    pub fn thumbnail(path: impl Into<PathBuf>, width: i32, height: i32) -> Self {
        Self {
            thumbnail: true,
            ..Self::bounded(path, width, height)
        }
    }
}

/// The decode size asked of the loader, at the image's own proportions: vector
/// formats render at the bound, raster formats fit within it and never upscale.
///
/// Glycin scales to exactly the size asked, so the bound itself would squeeze a
/// picture square. With `natural` unknown the bound is all there is, which
/// gdk-pixbuf fits into at the right proportions.
#[must_use]
pub(crate) fn effective_decode_bound(
    bound: Option<(i32, i32)>,
    natural: Option<(i32, i32)>,
    scalable: bool,
) -> Option<(i32, i32)> {
    let (bw, bh) = bound?;
    let (bw, bh) = (bw.max(1), bh.max(1));
    let Some((nw, nh)) = natural.filter(|(nw, nh)| *nw > 0 && *nh > 0) else {
        return Some((bw, bh));
    };
    if !scalable {
        return Some(fit_within((nw, nh), (bw, bh)));
    }
    let scale = (f64::from(bw) / f64::from(nw)).min(f64::from(bh) / f64::from(nh));
    #[allow(
        clippy::cast_possible_truncation,
        reason = "at most the bound, which is an i32"
    )]
    let side = |natural: i32| ((f64::from(natural) * scale).round() as i32).max(1);
    Some((side(nw), side(nh)))
}

/// What one sandboxed open can say about a file without showing it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BigImageMetadata {
    /// The camera fields the file carries, empty when it carries none.
    pub exif: exif::BigExif,
    /// How many pixels the image really has, as the decoder read its header.
    ///
    /// This is the file's own size, not the size anything was displayed at.
    pub pixel_size: Option<(i32, i32)>,
    /// What a copy of the file carries over from it.
    pub carried: BigCarriedMetadata,
}

/// `path`, or the file it names when it is a symbolic link: the sandboxed
/// decoder opens no file through a link. Blocking.
#[must_use]
pub fn followed_link(path: PathBuf) -> PathBuf {
    if std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        std::fs::canonicalize(&path).unwrap_or(path)
    } else {
        path
    }
}

/// Read what a viewer displays about a file, through the decoding sandbox.
///
/// Runs on the calling thread; call it from a worker.
///
/// # Errors
///
/// Returns [`BigImageError`] when the sandbox is unavailable or the file
/// cannot be staged and opened.
pub fn read_metadata_sandbox_required(
    path: &std::path::Path,
) -> Result<BigImageMetadata, BigImageError> {
    secure::read_metadata_sandbox_required(path)
}

/// Decode `spec` in the Glycin sandbox.
///
/// Validates a bounded local regular file and fails closed when Glycin can
/// confine no loader. This function runs on the calling thread; UI code
/// should prefer [`load_sandboxed_texture`].
///
/// # Errors
///
/// Fails when the sandbox is unavailable, the source is unsafe or past the
/// size limits, or the sandbox cannot decode the image.
pub fn decode_rgba_sandboxed(spec: &BigImageSpec) -> Result<BigRgbaImage, BigImageError> {
    secure::decode_sandbox_required(spec)
}

/// Decode `spec` in the sandbox for a save, whose frame may have up to
/// `max_pixels` (see [`BigSaveTurn::pixel_ceiling`]) instead of a display frame's
/// ceiling. The bound may be any size: the frame fitted inside it is what is
/// held to `max_pixels`.
///
/// Runs on the calling thread; it belongs on a worker.
///
/// # Errors
///
/// As [`decode_rgba_sandboxed`], and [`BigImageError::PixelLimitExceeded`]
/// for a frame past `max_pixels`.
pub fn decode_rgba_for_save(
    spec: &BigImageSpec,
    max_pixels: u64,
) -> Result<BigRgbaImage, BigImageError> {
    secure::decode_for_save(spec, max_pixels)
}

/// Blocking [`decode_rgba_sandboxed`] that a `cancel_flag` can abort.
///
/// For a worker thread with no main context of its own (the viewer decodes on
/// GIO's blocking pool). It spins a private context, bridges the `Send` flag to
/// a [`gio::Cancellable`] the sandboxed decode honors, and returns
/// [`BigImageError::Cancelled`] when the caller sets the flag, so paging past
/// an image aborts its decode instead of running it to completion. The flag is
/// polled because an `AtomicBool` has no async wake; the poll ends with the
/// decode.
///
/// The frame comes with the size of the image it was decoded from, which a
/// viewer measures its zoom against.
///
/// A loader that cannot reduce while decoding delivers the source at full size
/// before it is scaled down; that frame is charged to `resources` as `class`
/// for as long as it lives, on top of the display frame the caller admitted;
/// `held` is the size of that display frame, which a frame larger than the
/// governor leaves out of its charge.
///
/// # Errors
///
/// The failures [`decode_rgba_sandboxed`] reports, plus
/// [`BigImageError::Cancelled`] and [`BigImageError::ResourceLimit`] when the
/// full frame does not fit.
pub fn decode_rgba_sandboxed_cancellable(
    spec: &BigImageSpec,
    cancel_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    resources: &std::sync::Arc<big_os_kit::session_resources::SessionResources>,
    class: big_os_kit::session_resources::MemoryClass,
    held: usize,
) -> Result<(BigRgbaImage, BigSourceSize), BigImageError> {
    let spec = spec.clone();
    glib::MainContext::new().block_on(async move {
        let cancellable = gio::Cancellable::new();
        let poll = cancellable.clone();
        let poll_task = glib::spawn_future_local(async move {
            while !poll.is_cancelled() {
                if cancel_flag.load(std::sync::atomic::Ordering::Acquire) {
                    poll.cancel();
                    break;
                }
                glib::timeout_future(std::time::Duration::from_millis(30)).await;
            }
        });
        let whole_frame = secure::WholeFrameAdmission {
            resources,
            class,
            held,
        };
        let result =
            secure::decode_sandbox_required_async(&spec, Some(cancellable), Some(whole_frame))
                .await;
        // Dropping a GLib JoinHandle detaches it; explicitly stop the bridge.
        poll_task.abort();
        result
    })
}

/// Decode `spec` awaiting on the caller's context instead of blocking a thread.
///
/// The sandboxed decode happens in the loader process either way; what changes
/// is that no thread here is turned into a main loop to wait for it. GUI code
/// should call this from the main context and leave
/// [`decode_rgba_sandboxed`] to callers that have no context of their own.
///
/// # Errors
///
/// The same failures [`decode_rgba_sandboxed`] reports.
pub async fn decode_rgba_sandboxed_async(
    spec: &BigImageSpec,
) -> Result<BigRgbaImage, BigImageError> {
    decode_rgba_sandboxed_cancellable_async(spec, None).await
}

/// Like [`decode_rgba_sandboxed_async`], but a `cancellable` aborts the decode.
///
/// Cancelling frees the worker slot and the loader at once, for a list scrolled
/// or a viewer paged past an image before its decode finished.
///
/// # Errors
///
/// The same failures [`decode_rgba_sandboxed`] reports, plus
/// [`BigImageError::Cancelled`] when `cancellable` is triggered.
pub async fn decode_rgba_sandboxed_cancellable_async(
    spec: &BigImageSpec,
    cancellable: Option<gio::Cancellable>,
) -> Result<BigRgbaImage, BigImageError> {
    secure::decode_sandbox_required_async(spec, cancellable, None)
        .await
        .map(|(image, _)| image)
}

/// The picture an [`exif::EmbeddedThumbnail`] holds, fitted within `bound`
/// pixels and turned upright, through the decoding sandbox.
///
/// # Errors
///
/// The same failures [`decode_rgba_sandboxed_cancellable_async`] reports.
pub async fn decode_embedded_thumbnail_async(
    thumbnail: &exif::EmbeddedThumbnail<'_>,
    bound: i32,
    cancellable: Option<gio::Cancellable>,
) -> Result<BigRgbaImage, BigImageError> {
    let image =
        secure::decode_jpeg_bytes_async(thumbnail.jpeg.to_vec(), bound, cancellable).await?;
    // Mirrored orientations are left as they are: rare in camera files.
    match thumbnail.orientation {
        3 => rotate_rgba_quarter_turn(&image, BigImageQuarterTurn::Half),
        6 => rotate_rgba_quarter_turn(&image, BigImageQuarterTurn::Clockwise),
        8 => rotate_rgba_quarter_turn(&image, BigImageQuarterTurn::CounterClockwise),
        _ => Ok(image),
    }
}

/// Size glycin's executor before glycin starts it.
///
/// glycin runs its I/O on async-global-executor, which starts one thread per
/// core the first time it is asked, once per copy linked into the host. Loading
/// an image is mostly waiting on the sandboxed loader, so two are enough. The
/// first call of each copy wins; every entry into glycin here makes it first.
pub(crate) fn glycin_threads() {
    static SIZED: std::sync::Once = std::sync::Once::new();
    SIZED.call_once(|| {
        async_global_executor::init_with_config(
            async_global_executor::GlobalExecutorConfig::default()
                .with_min_threads(1)
                .with_max_threads(2),
        );
    });
}

pub(crate) fn large_jpeg_source_enabled() -> bool {
    jpeg_loader::large_jpeg_source()
}

/// The frame's color space, or `None` when it is plain sRGB.
///
/// `None` is not "unknown": it is the one case where handing the pixels to GDK
/// untagged is exactly right, and it keeps the common path allocation-free.
pub(crate) fn frame_cicp(frame: &glycin::Frame) -> Option<BigCicp> {
    let glycin::ColorState::Cicp(cicp) = frame.color_state() else {
        return None;
    };
    Some(BigCicp {
        color_primaries: u8::from(cicp.color_primaries).into(),
        transfer_characteristics: u8::from(cicp.transfer_characteristics).into(),
        matrix_coefficients: u8::from(cicp.matrix_coefficients).into(),
        full_range: u8::from(cicp.video_full_range_flag) == 1,
    })
}

/// Whether an ICC profile names itself sRGB in its description: glycin
/// converts such a frame to sRGB without changing a color.
pub(crate) fn names_srgb(icc: &[u8]) -> bool {
    // The tag table follows the 128-byte header: a count, then a signature,
    // offset and size for each tag.
    let count = be_u32(icc, 128).unwrap_or(0);
    let Some(desc) = (0..count.min(icc.len() / 12))
        .map(|entry| 132 + entry * 12)
        .find(|&at| icc.get(at..at + 4) == Some(b"desc"))
    else {
        return false;
    };
    let tag = be_u32(icc, desc + 4)
        .zip(be_u32(icc, desc + 8))
        .and_then(|(offset, size)| icc.get(offset..offset.checked_add(size)?));
    let text = match tag.and_then(|tag| Some((tag.get(..4)?, tag))) {
        // Version 2: a length, then ASCII.
        Some((b"desc", tag)) => be_u32(tag, 8)
            .and_then(|length| tag.get(12..12_usize.checked_add(length)?))
            .map(|ascii| String::from_utf8_lossy(ascii).into_owned()),
        // Version 4: records of length and offset into UTF-16BE text; the
        // first one is enough.
        Some((b"mluc", tag)) => be_u32(tag, 20)
            .zip(be_u32(tag, 24))
            .and_then(|(length, offset)| tag.get(offset..offset.checked_add(length)?))
            .map(|utf16| {
                let units: Vec<u16> = utf16
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&unit| u16::from_be_bytes(unit))
                    .collect();
                String::from_utf16_lossy(&units)
            }),
        _ => None,
    };
    // "sRGB IEC61966-2.1", "sRGB2014", "GIMP built-in sRGB"; not "esRGB".
    text.is_some_and(|text| text.split_whitespace().any(|word| word.starts_with("sRGB")))
}

/// The big-endian `u32` at `at` in `bytes`.
fn be_u32(bytes: &[u8], at: usize) -> Option<usize> {
    let word = bytes.get(at..at.checked_add(4)?)?;
    usize::try_from(u32::from_be_bytes(word.try_into().ok()?)).ok()
}

/// Load a local image directly into a sandbox-backed `GdkTexture`.
///
/// Unlike [`load_sandboxed_texture`], this path does not normalize the Glycin
/// frame through an owned RGBA `Vec` first. Glycin's frame bytes are wrapped
/// directly by `GdkMemoryTexture`, so a 4K wallpaper does not temporarily need
/// a second full-size pixel buffer in the shell. The caller supplies a
/// [`gio::Cancellable`]; cancelling it tells Glycin to stop an obsolete decode
/// and this function returns [`BigImageError::Cancelled`].
///
/// `spec.bound` is required. The bound is interpreted using `fit`, and source
/// plus delivered-frame pixel limits are checked before the texture is returned.
///
/// # Errors
///
/// Fails closed when a Glycin sandbox is unavailable, the source is unsafe, the
/// source/frame exceeds the configured limits, the operation is cancelled, or
/// the sandbox cannot decode the image.
pub async fn load_sandboxed_texture_cancellable(
    spec: BigImageSpec,
    fit: BigImageFit,
    cancellable: gio::Cancellable,
) -> Result<gdk::Texture, BigImageError> {
    secure::load_texture_sandbox_required_async(spec, fit, cancellable).await
}

/// Like [`load_sandboxed_texture_cancellable`], but for a surface that keeps the
/// image resident and never transforms it (the desktop wallpaper): the pixels go
/// to a GPU-only [`gdk::GLTexture`] built in `context`, so they are not also held
/// in a CPU buffer. Only worthwhile under the GL renderer; falls back to a CPU
/// texture when the GL upload cannot be done.
///
/// # Errors
///
/// Same failures as [`load_sandboxed_texture_cancellable`].
pub async fn load_sandboxed_gl_texture_cancellable(
    spec: BigImageSpec,
    fit: BigImageFit,
    cancellable: gio::Cancellable,
    context: gdk::GLContext,
) -> Result<gdk::Texture, BigImageError> {
    secure::load_gl_texture_sandbox_required_async(spec, fit, cancellable, context).await
}

/// Load `spec` through the sandbox, awaiting on the caller's context.
///
/// The decode itself runs in the sandbox process; this awaits it rather than
/// parking a worker thread on a private main loop, and wraps the pixels into a
/// texture afterwards.
///
/// # Errors
///
/// The same failures [`decode_rgba_sandboxed`] reports, and
/// [`BigImageError::InvalidBytePixels`] when the pixels make no texture.
pub async fn load_sandboxed_texture(spec: BigImageSpec) -> Result<gdk::Texture, BigImageError> {
    let decoded = std::sync::Arc::new(decode_rgba_sandboxed_async(&spec).await?);
    rgba_to_shared_texture(&decoded).ok_or(BigImageError::InvalidBytePixels)
}

/// Pack a `Pixbuf` into [`BigRgbaImage`] (worker-thread safe: touches only
/// the pixbuf and a plain buffer). Feeding GTK the pixbuf's padded rowstride
/// (or borrowing its pixel bytes) is a layout/lifetime mismatch that can
/// corrupt the heap, so rows are copied into a packed owned buffer. An RGB
/// pixbuf is widened as it is copied, with no second pixbuf, and comes back
/// [`BigRgbaImage::opaque`].
#[must_use]
pub fn pixbuf_to_rgba(pixbuf: &gtk::gdk_pixbuf::Pixbuf) -> Option<BigRgbaImage> {
    let stride = usize::try_from(pixbuf.rowstride()).ok()?;
    let pixels = pixbuf.read_pixel_bytes();
    match (pixbuf.n_channels(), pixbuf.has_alpha()) {
        (4, true) => pack_rows_to_rgba(&pixels, pixbuf.width(), pixbuf.height(), stride),
        (3, false) => pack_rgb_rows_to_rgbx(&pixels, pixbuf.width(), pixbuf.height(), stride),
        _ => None,
    }
}

fn pack_rows_to_rgba(
    source: &[u8],
    width: i32,
    height: i32,
    source_stride: usize,
) -> Option<BigRgbaImage> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let row = usize::try_from(width).ok()?.checked_mul(4)?;
    let rows = usize::try_from(height).ok()?;
    let mut packed = vec![0_u8; row.checked_mul(rows)?];
    for y in 0..rows {
        let source_offset = y.checked_mul(source_stride)?;
        let packed_offset = y * row;
        packed
            .get_mut(packed_offset..packed_offset + row)?
            .copy_from_slice(source.get(source_offset..source_offset + row)?);
    }
    Some(BigRgbaImage {
        rgba: packed,
        width,
        height,
        cicp: None,
        opaque: false,
    })
}

/// Pack three-byte RGB rows into a four-byte RGBX buffer (alpha byte `255`).
///
/// The image keeps the one four-byte layout the rest of the crate expects
/// (rotation, crop, save all assume it), but its texture is tagged `R8g8b8x8`
/// so the padding byte is never sampled.
fn pack_rgb_rows_to_rgbx(
    source: &[u8],
    width: i32,
    height: i32,
    source_stride: usize,
) -> Option<BigRgbaImage> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let width = usize::try_from(width).ok()?;
    let rows = usize::try_from(height).ok()?;
    let source_row = width.checked_mul(3)?;
    let dest_row = width.checked_mul(4)?;
    let mut packed = vec![0_u8; dest_row.checked_mul(rows)?];
    let pack_row = rgb_row_packer();
    for y in 0..rows {
        let source_offset = y.checked_mul(source_stride)?;
        let source_row_bytes = source.get(source_offset..source_offset + source_row)?;
        let dest_offset = y * dest_row;
        let dest_row_bytes = packed.get_mut(dest_offset..dest_offset + dest_row)?;
        pack_row(source_row_bytes, dest_row_bytes);
    }
    Some(BigRgbaImage {
        rgba: packed,
        width: i32::try_from(width).ok()?,
        height: i32::try_from(rows).ok()?,
        cicp: None,
        opaque: true,
    })
}

/// The fastest row packer this processor runs. The compiler does not turn
/// the plain loop into byte shuffles for x86-64's baseline (SSE2), and SSSE3
/// is absent from some processors still in use (AMD's K10), so it is chosen
/// here at run time.
fn rgb_row_packer() -> fn(&[u8], &mut [u8]) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("ssse3") {
        // SAFETY: SSSE3 was just detected on this processor.
        return |source, dest| unsafe { pack_rgb_row_ssse3(source, dest) };
    }
    pack_rgb_row
}

/// One row of RGB pixels into RGBX, as many pixels as both rows hold.
fn pack_rgb_row(source: &[u8], dest: &mut [u8]) {
    for (rgb, rgbx) in source
        .as_chunks::<3>()
        .0
        .iter()
        .zip(dest.as_chunks_mut::<4>().0.iter_mut())
    {
        *rgbx = [rgb[0], rgb[1], rgb[2], 255];
    }
}

/// [`pack_rgb_row`] four pixels at a time: 16 bytes read, the 12 of the
/// pixels spread into 16 by one shuffle, and the alpha bytes set.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
fn pack_rgb_row_ssse3(source: &[u8], dest: &mut [u8]) {
    use std::arch::x86_64::{
        _mm_loadu_si128, _mm_or_si128, _mm_set1_epi32, _mm_setr_epi8, _mm_shuffle_epi8,
        _mm_storeu_si128,
    };
    let spread = _mm_setr_epi8(0, 1, 2, -1, 3, 4, 5, -1, 6, 7, 8, -1, 9, 10, 11, -1);
    let alpha = _mm_set1_epi32(i32::from_le_bytes([0, 0, 0, 255]));
    let pixels = (source.len() / 3).min(dest.len() / 4);
    let mut at = 0;
    // The load takes 4 bytes past the four pixels, so it stops while the row
    // still has them; the last pixels go through the plain loop.
    while (at + 4) * 3 + 4 <= pixels * 3 {
        #[expect(clippy::expect_used, reason = "the slice is 16 bytes long")]
        let input: &[u8; 16] = source[at * 3..at * 3 + 16]
            .try_into()
            .expect("16 bytes sliced");
        #[expect(clippy::expect_used, reason = "the slice is 16 bytes long")]
        let output: &mut [u8; 16] = (&mut dest[at * 4..at * 4 + 16])
            .try_into()
            .expect("16 bytes sliced");
        // SAFETY: both pointers come from 16-byte arrays borrowed above, and
        // the unaligned load and store need no alignment.
        unsafe {
            let packed = _mm_or_si128(
                _mm_shuffle_epi8(_mm_loadu_si128(input.as_ptr().cast()), spread),
                alpha,
            );
            _mm_storeu_si128(output.as_mut_ptr().cast(), packed);
        }
        at += 4;
    }
    pack_rgb_row(&source[at * 3..pixels * 3], &mut dest[at * 4..pixels * 4]);
}

/// Wrap packed RGBA into a [`gdk::MemoryTexture`]. GTK thread only.
///
/// Copies the pixels into the texture. Use [`rgba_to_shared_texture`] when the
/// caller keeps the decoded image alive too, so the pixels are held once.
#[must_use]
pub fn rgba_to_texture(image: &BigRgbaImage) -> Option<gdk::Texture> {
    let stride = texture_stride(image)?;
    let bytes = glib::Bytes::from(&image.rgba);
    Some(texture_from_bytes(&bytes, image, stride))
}

/// Like [`rgba_to_texture`], but the texture shares `image`'s pixel buffer
/// instead of copying it, keeping the `Arc` alive for the texture's lifetime.
///
/// For the one large image a viewer keeps decoded *and* displays: copying would
/// hold `width·height·4` bytes twice: 64 MiB each for a 4096² frame.
/// Accepts ordinary pixels or [`AccountedRgba`]; in the latter case the texture
/// also owns the admission lease until its last pixel consumer disappears.
#[must_use]
pub fn rgba_to_shared_texture<T>(image: &std::sync::Arc<T>) -> Option<gdk::Texture>
where
    T: AsRef<BigRgbaImage> + Send + Sync + 'static,
{
    let pixels = image.as_ref().as_ref();
    let stride = texture_stride(pixels)?;
    let bytes = glib::Bytes::from_owned(SharedRgba(std::sync::Arc::clone(image)));
    Some(texture_from_bytes(&bytes, pixels, stride))
}

/// The row stride in bytes, or `None` when the buffer does not match its size.
fn texture_stride(image: &BigRgbaImage) -> Option<usize> {
    if image.width <= 0 || image.height <= 0 {
        return None;
    }
    let width = usize::try_from(image.width).ok()?;
    let expected = width
        .checked_mul(usize::try_from(image.height).ok()?)?
        .checked_mul(4)?;
    if image.rgba.len() != expected {
        return None;
    }
    width.checked_mul(4)
}

fn texture_from_bytes(bytes: &glib::Bytes, image: &BigRgbaImage, stride: usize) -> gdk::Texture {
    // An opaque source keeps its four-byte layout but is tagged `R8g8b8x8`, so
    // GDK ignores the dead alpha: no premultiply pass on upload, no blend in the
    // compositor, and the texture can be offloaded to a subsurface.
    let format = if image.opaque {
        gdk::MemoryFormat::R8g8b8x8
    } else {
        gdk::MemoryFormat::R8g8b8a8
    };
    // Untagged pixels are treated as sRGB by GDK, which is right for sRGB and
    // wrong for everything else. When the decoder named a color space, the
    // texture carries it and the compositor converts for the real display.
    if let Some(color_state) = image.cicp.and_then(color_state_for) {
        return gdk::MemoryTextureBuilder::new()
            .set_width(image.width)
            .set_height(image.height)
            .set_format(format)
            .set_bytes(Some(bytes))
            .set_stride(stride)
            .set_color_state(&color_state)
            .build();
    }
    gdk::MemoryTexture::new(image.width, image.height, format, bytes, stride).upcast()
}

/// Keeps a decoded image's `Arc` alive as the backing store of a `glib::Bytes`,
/// so the texture and the retained pixels are one allocation, not two.
struct SharedRgba<T>(std::sync::Arc<T>);

impl<T: AsRef<BigRgbaImage>> AsRef<[u8]> for SharedRgba<T> {
    fn as_ref(&self) -> &[u8] {
        &self.0.as_ref().as_ref().rgba
    }
}

/// Turn CICP code points into the color state GDK understands.
///
/// A combination GDK rejects yields `None`, and the texture is then built
/// untagged: showing the image as sRGB is wrong, but showing nothing is worse,
/// and a refusal here would take a whole photograph off the screen over its
/// metadata.
fn color_state_for(cicp: BigCicp) -> Option<gdk::ColorState> {
    let params = gdk::CicpParams::new();
    params.set_color_primaries(cicp.color_primaries);
    params.set_transfer_function(cicp.transfer_characteristics);
    params.set_matrix_coefficients(cicp.matrix_coefficients);
    params.set_range(if cicp.full_range {
        gdk::CicpRange::Full
    } else {
        gdk::CicpRange::Narrow
    });
    params.build_color_state().ok()
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
