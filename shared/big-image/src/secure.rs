// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! Fail-closed local image validation and sandboxed decoding.

use std::ffi::{CString, OsString};
use std::fs::{File, OpenOptions};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path};

use gtk::gio::prelude::*;
use gtk::{gdk, gio, glib};

use big_os_kit::session_resources::{MemoryClass, MemoryDomain, MemoryLease, SessionResources};
use std::sync::Arc;

use super::{BigImageError, BigImageFit, BigImageSpec, BigRgbaImage};
use super::{effective_decode_bound, pack_rgb_rows_to_rgbx, pack_rows_to_rgba};

const MAX_SOURCE_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const MAX_SOURCE_PIXELS: u64 = 256_000_000;
/// Source-pixel cap when a reduce-decoding JPEG loader is enabled: the JPEG
/// format's own maximum dimensions (65535²). The loader hands back a small
/// frame, so the source size does not imply the memory cost.
const LARGE_JPEG_MAX_SOURCE_PIXELS: u64 = 65_535 * 65_535;
pub(crate) const MAX_DISPLAY_PIXELS: u64 = 64_000_000;

pub(super) struct StagedRequest {
    source: StagedSource,
    bound: (i32, i32),
    /// Source sub-rectangle to decode, carried from [`BigImageSpec::clip`].
    clip: Option<(u32, u32, u32, u32)>,
    /// The most pixels the delivered frame may have.
    max_pixels: u64,
    /// From [`BigImageSpec::thumbnail`].
    thumbnail: bool,
}

struct StagedSource {
    /// The validated source, held open for as long as anything may decode it:
    /// `decode_path` resolves through `/proc/self/fd`, so closing this would
    /// turn the symlink into a dangling one.
    #[expect(dead_code, reason = "held open so /proc/self/fd/N keeps resolving")]
    file: File,
    decode_path: std::path::PathBuf,
    private_directory: std::path::PathBuf,
}

impl StagedSource {
    fn unlink_decode_path(&self) {
        let _ = std::fs::remove_file(&self.decode_path);
        let _ = std::fs::remove_dir(&self.private_directory);
    }
}

impl Drop for StagedSource {
    fn drop(&mut self) {
        self.unlink_decode_path();
    }
}

pub(super) fn decode_sandbox_required(spec: &BigImageSpec) -> Result<BigRgbaImage, BigImageError> {
    let mechanism = glib::MainContext::new().block_on(glycin::SandboxMechanism::detect());
    if mechanism == glycin::SandboxMechanism::NotSandboxed {
        return Err(BigImageError::SandboxUnavailable);
    }
    let staged = stage_request(spec)?;
    decode_validated(&staged, mechanism)
}

/// A decode for a save: the bound may be any size, and the frame delivered
/// may reach `max_pixels`, up to the source ceiling.
pub(super) fn decode_for_save(
    spec: &BigImageSpec,
    max_pixels: u64,
) -> Result<BigRgbaImage, BigImageError> {
    let mechanism = glib::MainContext::new().block_on(glycin::SandboxMechanism::detect());
    if mechanism == glycin::SandboxMechanism::NotSandboxed {
        return Err(BigImageError::SandboxUnavailable);
    }
    let staged = stage_request_within(spec, u64::MAX, max_pixels.min(MAX_SOURCE_PIXELS))?;
    decode_validated(&staged, mechanism)
}

/// Run blocking `work` where it cannot hold the main context: on GIO's pool
/// when awaited there, inline anywhere else. Anywhere else is a worker spinning
/// a private context for a blocking decode, and a pool task completes through
/// an idle on the context it was started from: once that context stops, the
/// idle never runs, and the task and the context keep each other alive.
async fn off_main_context<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, BigImageError> {
    if glib::MainContext::ref_thread_default() == glib::MainContext::default() {
        gio::spawn_blocking(work)
            .await
            .map_err(|_| BigImageError::WorkerGone)
    } else {
        Ok(work())
    }
}

/// The same decode, awaited on the caller's own context.
///
/// Glycin is asynchronous and does its work in another process; the blocking
/// twin above reaches that by spinning a private `GMainContext`, which turns
/// whatever thread it runs on into a main loop. On a worker from the thread
/// pool that is a loop dispatching library callbacks off the main thread,
/// which is where a GTK application must never end up. Callers with a main
/// context (every GUI one) should await this instead.
pub(super) async fn decode_sandbox_required_async(
    spec: &BigImageSpec,
    cancellable: Option<gio::Cancellable>,
    whole_frame: Option<WholeFrameAdmission<'_>>,
) -> Result<(BigRgbaImage, super::BigSourceSize), BigImageError> {
    if cancellable
        .as_ref()
        .is_some_and(gio::Cancellable::is_cancelled)
    {
        return Err(BigImageError::Cancelled);
    }
    let mechanism = glycin::SandboxMechanism::detect().await;
    if mechanism == glycin::SandboxMechanism::NotSandboxed {
        return Err(BigImageError::SandboxUnavailable);
    }
    // Opening/stat'ing a user-selected path can block on FUSE or a slow mount, so
    // the fd-pinning staging stays off the main context. Thumbnails await this
    // RGBA path there (via `spawn_future_local`), so staging inline would jam the
    // GTK main loop on every thumbnail miss.
    let spec = spec.clone();
    let staged = off_main_context(move || stage_request(&spec)).await??;
    let mut loader = staged_loader(&staged);
    // Wiring the cancellable into the loader lets a scroll or navigation that
    // moved on abort the sandboxed decode instead of running it to completion
    // and dropping the result.
    if let Some(cancellable) = &cancellable {
        loader.cancellable(cancellable.clone());
    }
    let decoded = decode_sandboxed(
        loader,
        staged.bound,
        staged.clip,
        staged.max_pixels,
        mechanism,
        cancellable.as_ref(),
        whole_frame,
    )
    .await;
    staged.source.unlink_decode_path();
    decoded
}

/// The sandboxed decode of a JPEG that is in memory, such as a thumbnail
/// pulled out of another file.
pub(super) async fn decode_jpeg_bytes_async(
    jpeg: Vec<u8>,
    bound: i32,
    cancellable: Option<gio::Cancellable>,
) -> Result<BigRgbaImage, BigImageError> {
    if cancellable
        .as_ref()
        .is_some_and(gio::Cancellable::is_cancelled)
    {
        return Err(BigImageError::Cancelled);
    }
    let bound = validate_bound(Some((bound, bound)), MAX_DISPLAY_PIXELS)?;
    let mechanism = glycin::SandboxMechanism::detect().await;
    if mechanism == glycin::SandboxMechanism::NotSandboxed {
        return Err(BigImageError::SandboxUnavailable);
    }
    crate::glycin_threads();
    let mut loader = glycin::Loader::new_bytes(glib::Bytes::from_owned(jpeg));
    loader.pool(thumbnail_pool());
    if let Some(cancellable) = &cancellable {
        loader.cancellable(cancellable.clone());
    }
    decode_sandboxed(
        loader,
        bound,
        None,
        MAX_DISPLAY_PIXELS,
        mechanism,
        cancellable.as_ref(),
        None,
    )
    .await
    .map(|(image, _)| image)
}

pub(super) async fn load_texture_sandbox_required_async(
    spec: BigImageSpec,
    fit: BigImageFit,
    cancellable: gio::Cancellable,
) -> Result<gdk::Texture, BigImageError> {
    let (frame, (target_width, target_height)) =
        sandboxed_frame(spec, fit, &cancellable, true).await?;
    let frame_width = i32::try_from(frame.width()).map_err(|_| BigImageError::InvalidBytePixels)?;
    let frame_height =
        i32::try_from(frame.height()).map_err(|_| BigImageError::InvalidBytePixels)?;
    if (frame_width > target_width || frame_height > target_height)
        && frame.memory_format() == glycin::MemoryFormat::R16g16b16
    {
        // The system's HEIF and image-rs loaders deliver an HDR picture whole,
        // whatever size is asked, and gdk-pixbuf scales eight bits only.
        let color_state = super::frame_cicp(&frame).and_then(super::color_state_for);
        let (width, height) = (target_width, target_height);
        let bytes = gio::spawn_blocking(move || {
            let size = |side: u32| usize::try_from(side).ok();
            let shrunk = super::shrink_rgb16(
                frame.buf_slice(),
                size(frame.stride())?,
                (size(frame.width())?, size(frame.height())?),
                (usize::try_from(width).ok()?, usize::try_from(height).ok()?),
            );
            drop(frame);
            shrunk
        })
        .await
        .map_err(|_| BigImageError::WorkerGone)?
        .ok_or(BigImageError::InvalidBytePixels)?;
        let mut builder = gdk::MemoryTextureBuilder::new()
            .set_width(width)
            .set_height(height)
            .set_format(gdk::MemoryFormat::R16g16b16)
            .set_bytes(Some(&glib::Bytes::from_owned(bytes)))
            .set_stride(usize::try_from(width).map_err(|_| BigImageError::InvalidBytePixels)? * 6);
        if let Some(color_state) = color_state {
            builder = builder.set_color_state(&color_state);
        }
        return Ok(builder.build());
    }
    if frame_width > target_width || frame_height > target_height {
        // Old glycin ignored the scale hint and delivered a full-resolution
        // frame; the direct texture would then keep every source pixel (an 8000px
        // photo behind a 1080p wallpaper). Downscale to the fit-aware target,
        // preserving the frame's color state, instead of the fast zero-copy wrap.
        // On a worker: this is awaited on the caller's main context.
        let image =
            gio::spawn_blocking(move || frame_to_rgba_scaled(&frame, target_width, target_height))
                .await
                .map_err(|_| BigImageError::WorkerGone)??;
        return super::rgba_to_texture(&image).ok_or(BigImageError::InvalidBytePixels);
    }
    Ok(frame.texture())
}

/// Stage `spec`, decode one frame of it in the sandbox at `fit`, and remove
/// the staged copy. `hdr`: an opaque PQ or HLG picture keeps its 16 bits, for
/// a surface that draws it in its own color state.
async fn sandboxed_frame(
    spec: BigImageSpec,
    fit: BigImageFit,
    cancellable: &gio::Cancellable,
    hdr: bool,
) -> Result<(glycin::Frame, (i32, i32)), BigImageError> {
    // Opening/stat'ing a user-selected path can block on FUSE or a slow mount.
    // GIO's blocking pool is the correct place for that synchronous, fd-pinning
    // staging work; Glycin itself is asynchronous after this point.
    let staged = gio::spawn_blocking(move || stage_request(&spec))
        .await
        .map_err(|_| BigImageError::WorkerGone)??;
    if cancellable.is_cancelled() {
        return Err(BigImageError::Cancelled);
    }
    let mechanism = glycin::SandboxMechanism::detect().await;
    if mechanism == glycin::SandboxMechanism::NotSandboxed {
        return Err(BigImageError::SandboxUnavailable);
    }
    let mut loader = staged_loader(&staged);
    loader.cancellable(cancellable.clone());
    if hdr {
        loader.accepted_hdr_memory_formats(glycin::MemoryFormatSelection::R16g16b16);
    }
    let frame = load_frame_sandboxed(
        loader,
        staged.bound,
        staged.clip,
        staged.max_pixels,
        fit,
        mechanism,
        Some(cancellable),
        None,
    )
    .await;
    staged.source.unlink_decode_path();
    let (frame, target, _, _) = frame?;
    if cancellable.is_cancelled() {
        return Err(BigImageError::Cancelled);
    }
    Ok((frame, target))
}

/// Like [`load_texture_sandbox_required_async`], but for a surface that keeps the
/// image resident and never transforms it: the pixels go straight to a GPU-only
/// [`gdk::GLTexture`] and the CPU buffer is dropped, so the image is not held
/// twice. Falls back to a CPU texture when the GL upload cannot be done.
pub(super) async fn load_gl_texture_sandbox_required_async(
    spec: BigImageSpec,
    fit: BigImageFit,
    cancellable: gio::Cancellable,
    context: gdk::GLContext,
) -> Result<gdk::Texture, BigImageError> {
    let (frame, (target_width, target_height)) =
        sandboxed_frame(spec, fit, &cancellable, false).await?;
    // On a worker: this is awaited on the caller's main context, and when
    // glycin ignores the size asked for, packing is a full-resolution scale.
    let image = gio::spawn_blocking(move || frame_to_rgba(&frame, target_width, target_height))
        .await
        .map_err(|_| BigImageError::WorkerGone)??;
    let image = std::sync::Arc::new(image);
    // Prefer the GPU-only texture; if the GL upload fails, share the CPU buffer
    // with a memory texture rather than failing the wallpaper entirely.
    super::gl_texture::rgba_to_gl_texture(&image, &context)
        .or_else(|| super::rgba_to_shared_texture(&image))
        .ok_or(BigImageError::InvalidBytePixels)
}

/// Read the displayable EXIF fields through the same sandbox a decode uses.
///
/// A separate path: the decode runs for every image the user pages through,
/// and only the Properties dialog reads metadata.
pub(super) fn read_metadata_sandbox_required(
    path: &Path,
) -> Result<super::BigImageMetadata, BigImageError> {
    let mechanism = glib::MainContext::new().block_on(glycin::SandboxMechanism::detect());
    if mechanism == glycin::SandboxMechanism::NotSandboxed {
        return Err(BigImageError::SandboxUnavailable);
    }
    validate_path_shape(path)?;
    let source = stage_source(path, MAX_SOURCE_BYTES)?;
    glib::MainContext::new().block_on(async move {
        crate::glycin_threads();
        let mut loader = glycin::Loader::new(gio::File::for_path(&source.decode_path));
        loader.sandbox_selector(mechanism.into_selector());
        let image = loader
            .load()
            .await
            .map_err(|_| BigImageError::SandboxedDecodeFailed)?;
        if image.active_sandbox_mechanism() == glycin::SandboxMechanism::NotSandboxed {
            return Err(BigImageError::SandboxUnavailable);
        }
        // The size the file really has, taken from the same open: the pixels
        // a viewer shows were scaled to its window, so they answer a different
        // question than "how big is this image".
        let details = image.details();
        let pixel_size = i32::try_from(details.width())
            .ok()
            .zip(i32::try_from(details.height()).ok())
            .filter(|(width, height)| *width > 0 && *height > 0);
        // Set only by our JPEG loader, from the file's quantization table.
        let jpeg_quality = details
            .metadata_key_value()
            .and_then(|values| values.get("jpeg-quality")?.parse::<u8>().ok());
        let Some(blob) = details.metadata_exif() else {
            return Ok(super::BigImageMetadata {
                exif: crate::exif::BigExif::default(),
                pixel_size,
                carried: super::BigCarriedMetadata {
                    exif: None,
                    jpeg_quality,
                },
            });
        };
        let bytes = blob
            .get_full()
            .map_err(|_| BigImageError::SandboxedDecodeFailed)?;
        Ok(super::BigImageMetadata {
            exif: crate::exif::read_exif(&bytes),
            pixel_size,
            carried: super::BigCarriedMetadata {
                exif: Some(bytes),
                jpeg_quality,
            },
        })
    })
}

fn stage_request(spec: &BigImageSpec) -> Result<StagedRequest, BigImageError> {
    stage_request_within(spec, MAX_DISPLAY_PIXELS, MAX_DISPLAY_PIXELS)
}

/// Stage `spec` with a bound of at most `max_bound` pixels, for a frame of at
/// most `max_pixels`.
fn stage_request_within(
    spec: &BigImageSpec,
    max_bound: u64,
    max_pixels: u64,
) -> Result<StagedRequest, BigImageError> {
    let bound = validate_bound(spec.bound, max_bound)?;
    validate_path_shape(&spec.path)?;
    let source = stage_source(&spec.path, MAX_SOURCE_BYTES)?;
    Ok(StagedRequest {
        source,
        bound,
        clip: spec.clip,
        max_pixels,
        thumbnail: spec.thumbnail,
    })
}

/// A loader for `staged`, from [`thumbnail_pool`] for a thumbnail.
fn staged_loader(staged: &StagedRequest) -> glycin::Loader {
    crate::glycin_threads();
    let mut loader = glycin::Loader::new(gio::File::for_path(&staged.source.decode_path));
    if staged.thumbnail {
        loader.pool(thumbnail_pool());
    }
    loader
}

/// The loaders thumbnails decode in, started at [`crate::THUMBNAIL_NICE`].
/// Kept apart from glycin's global pool, whose loaders a picture someone is
/// waiting for reuses at the desktop's own priority.
fn thumbnail_pool() -> Arc<glycin::Pool> {
    static POOL: std::sync::LazyLock<Arc<glycin::Pool>> = std::sync::LazyLock::new(|| {
        let mut config = glycin::PoolConfig::new();
        config.nice(crate::THUMBNAIL_NICE);
        glycin::Pool::new(config)
    });
    Arc::clone(&POOL)
}

/// Pin `path` by descriptor and expose it under a private, safely-named path.
///
/// glycin reads the source in this process and pipes it to the sandbox, but it
/// needs a basename: `guess_mime_type` falls back to the extension for TIFF,
/// SVG, SVGZ and XBM. So the private directory holds a symlink to
/// `/proc/self/fd/N`, which reaches exactly the inode opened and validated here.
///
/// This pins the file's identity, not its bytes: a writer holding the inode can
/// still change it during the decode. A caller that needs the pixels to match a
/// version re-checks identity around the decode, as the thumbnail pipeline does.
fn stage_source(path: &Path, max_bytes: u64) -> Result<StagedSource, BigImageError> {
    let source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| BigImageError::InvalidSource {
            reason: "the file cannot be opened safely",
        })?;
    let metadata = source
        .metadata()
        .map_err(|_| BigImageError::InvalidSource {
            reason: "the file cannot be inspected",
        })?;
    if !metadata.is_file() {
        return Err(BigImageError::InvalidSource {
            reason: "the path is not a regular file",
        });
    }
    if metadata.len() > max_bytes {
        return Err(BigImageError::SourceTooLarge { max_bytes });
    }

    link_private_decode_path(path, source).map_err(|_| BigImageError::InvalidSource {
        reason: "a private decode path could not be created",
    })
}

fn link_private_decode_path(source_path: &Path, file: File) -> std::io::Result<StagedSource> {
    use std::os::fd::AsRawFd as _;

    let private_directory = create_private_directory()?;
    let decode_path = private_directory.join(format!("image{}", safe_decode_suffix(source_path)));
    if let Err(error) =
        std::os::unix::fs::symlink(format!("/proc/self/fd/{}", file.as_raw_fd()), &decode_path)
    {
        let _ = std::fs::remove_dir(&private_directory);
        return Err(error);
    }
    Ok(StagedSource {
        file,
        decode_path,
        private_directory,
    })
}

fn create_private_directory() -> std::io::Result<std::path::PathBuf> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from);
    let mut last_error = None;
    for base in runtime_dir.into_iter().chain([std::env::temp_dir()]) {
        if !base.is_absolute() {
            continue;
        }
        let template = base.join("big-image-XXXXXX");
        let Ok(template) = CString::new(template.as_os_str().as_bytes()) else {
            continue;
        };
        let mut bytes = template.into_bytes_with_nul();
        // SAFETY: `bytes` is a writable NUL-terminated template ending in six X
        // bytes; `mkdtemp` mutates it in place and creates the directory 0700.
        let result = unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) };
        if result.is_null() {
            last_error = Some(std::io::Error::last_os_error());
            continue;
        }
        // SAFETY: successful `mkdtemp` returns the same live NUL-terminated buffer.
        let path = unsafe { std::ffi::CStr::from_ptr(result) };
        return Ok(std::path::PathBuf::from(OsString::from_vec(
            path.to_bytes().to_vec(),
        )));
    }
    Err(last_error.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no absolute private staging base is available",
        )
    }))
}

fn safe_decode_suffix(path: &Path) -> String {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return String::new();
    };
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".svg.gz") {
        return ".svg.gz".to_owned();
    }
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return String::new();
    };
    if extension.is_empty()
        || extension.len() > 16
        || !extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return String::new();
    }
    format!(".{}", extension.to_ascii_lowercase())
}

fn validate_bound(bound: Option<(i32, i32)>, max_pixels: u64) -> Result<(i32, i32), BigImageError> {
    let Some((width, height)) = bound else {
        return Err(BigImageError::InvalidDisplayBound);
    };
    let pixels = positive_pixel_count(width, height).ok_or(BigImageError::InvalidDisplayBound)?;
    if pixels > max_pixels {
        return Err(BigImageError::PixelLimitExceeded {
            width: u32::try_from(width).unwrap_or(u32::MAX),
            height: u32::try_from(height).unwrap_or(u32::MAX),
            max_pixels,
        });
    }
    Ok((width, height))
}

fn validate_path_shape(path: &Path) -> Result<(), BigImageError> {
    if path.as_os_str().is_empty() {
        return Err(BigImageError::InvalidSource {
            reason: "the path is empty",
        });
    }
    if looks_like_remote_uri(path) {
        return Err(BigImageError::InvalidSource {
            reason: "remote locations are not supported",
        });
    }
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err(BigImageError::InvalidSource {
            reason: "the path must be absolute and cannot contain parent traversal",
        });
    }
    Ok(())
}

fn looks_like_remote_uri(path: &Path) -> bool {
    let value = path.to_string_lossy();
    let Some((scheme, _)) = value.split_once("://") else {
        return false;
    };
    !scheme.is_empty()
        && scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
}

fn positive_pixel_count(width: i32, height: i32) -> Option<u64> {
    let width = u64::try_from(width).ok()?;
    let height = u64::try_from(height).ok()?;
    (width > 0 && height > 0)
        .then_some(width)
        .and_then(|width| width.checked_mul(height))
}

fn validate_source_dimensions(width: u32, height: u32) -> Result<(), BigImageError> {
    validate_dimensions(width, height, MAX_SOURCE_PIXELS)
}

fn validate_display_dimensions(width: u32, height: u32) -> Result<(), BigImageError> {
    validate_dimensions(width, height, MAX_DISPLAY_PIXELS)
}

fn validate_dimensions(width: u32, height: u32, max_pixels: u64) -> Result<(), BigImageError> {
    let pixels = u64::from(width).checked_mul(u64::from(height)).ok_or(
        BigImageError::PixelLimitExceeded {
            width,
            height,
            max_pixels,
        },
    )?;
    if width == 0 || height == 0 || pixels > max_pixels {
        return Err(BigImageError::PixelLimitExceeded {
            width,
            height,
            max_pixels,
        });
    }
    Ok(())
}

fn decode_validated(
    spec: &StagedRequest,
    mechanism: glycin::SandboxMechanism,
) -> Result<BigRgbaImage, BigImageError> {
    let bound = spec.bound;
    let loader = staged_loader(spec);
    let result = glib::MainContext::new().block_on(decode_sandboxed(
        loader,
        bound,
        spec.clip,
        spec.max_pixels,
        mechanism,
        None,
        None,
    ));
    spec.source.unlink_decode_path();
    result.map(|(image, _)| image)
}

/// A sandboxed decoder kept alive to serve the frames of an animation.
///
/// The staged copy is held with the decoder: glycin streams the file to the
/// sandbox as frames are asked for, so it must outlive the first frame.
pub(super) struct SandboxedAnimation {
    image: glycin::Image,
    bound: (i32, i32),
    _staged: StagedSource,
}

impl SandboxedAnimation {
    /// Open `spec` and take its first frame.
    ///
    /// Returns `Ok(None)` when the source is a still image: a decoder with no
    /// second frame is a sandbox process kept alive for nothing.
    pub(super) async fn open(
        spec: StagedRequest,
        mechanism: glycin::SandboxMechanism,
    ) -> Result<Option<(Self, super::BigAnimationFrame)>, BigImageError> {
        let mut loader = staged_loader(&spec);
        loader.sandbox_selector(mechanism.into_selector());
        loader.accepted_memory_formats(glycin::MemoryFormatSelection::R8g8b8a8);
        let image = loader
            .load()
            .await
            .map_err(|_| BigImageError::SandboxedDecodeFailed)?;
        if image.active_sandbox_mechanism() == glycin::SandboxMechanism::NotSandboxed {
            return Err(BigImageError::SandboxUnavailable);
        }
        let details = image.details();
        validate_source_dimensions(details.width(), details.height())?;
        let animation = Self {
            image,
            bound: validate_bound(Some(spec.bound), MAX_DISPLAY_PIXELS)?,
            _staged: spec.source,
        };
        let frame = animation.take_frame(true).await?;
        Ok(frame.delay.map(|_| (animation, frame)))
    }

    /// The next frame of the loop, which glycin restarts on its own.
    pub(super) async fn next_frame(&self) -> Result<super::BigAnimationFrame, BigImageError> {
        self.take_frame(true).await
    }

    /// The next frame for export, or `None` after the last one.
    ///
    /// Without the loop, glycin answers the frame after the last with a named
    /// `NoMoreFrames` error, which is what makes writing every frame exactly
    /// once possible at all.
    pub(super) async fn next_export_frame(
        &self,
    ) -> Result<Option<super::BigAnimationFrame>, BigImageError> {
        match self.take_frame(false).await {
            Ok(frame) => Ok(Some(frame)),
            Err(BigImageError::AnimationEnded) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn take_frame(
        &self,
        loop_animation: bool,
    ) -> Result<super::BigAnimationFrame, BigImageError> {
        let details = self.image.details();
        let natural = (
            i32::try_from(details.width()).map_err(|_| BigImageError::InvalidBytePixels)?,
            i32::try_from(details.height()).map_err(|_| BigImageError::InvalidBytePixels)?,
        );
        let (width, height) = effective_decode_bound(Some(self.bound), Some(natural), false)
            .ok_or(BigImageError::InvalidDisplayBound)?;
        let frame = self
            .image
            .specific_frame(
                glycin::FrameRequest::new()
                    .loop_animation(loop_animation)
                    .scale(
                        u32::try_from(width).map_err(|_| BigImageError::InvalidDisplayBound)?,
                        u32::try_from(height).map_err(|_| BigImageError::InvalidDisplayBound)?,
                    ),
            )
            .await
            .map_err(|error| {
                if error.error().is_no_more_frames() {
                    BigImageError::AnimationEnded
                } else {
                    BigImageError::SandboxedDecodeFailed
                }
            })?;
        validate_display_dimensions(frame.width(), frame.height())?;
        let frame_width =
            i32::try_from(frame.width()).map_err(|_| BigImageError::InvalidBytePixels)?;
        let frame_height =
            i32::try_from(frame.height()).map_err(|_| BigImageError::InvalidBytePixels)?;
        let stride =
            usize::try_from(frame.stride()).map_err(|_| BigImageError::InvalidBytePixels)?;
        let mut image = pack_rows_to_rgba(frame.buf_slice(), frame_width, frame_height, stride)
            .ok_or(BigImageError::InvalidBytePixels)?;
        image.cicp = super::frame_cicp(&frame);
        Ok(super::BigAnimationFrame {
            image,
            delay: super::displayed_delay(frame.delay()),
        })
    }
}

/// Stage `spec` for an animated decode; blocking, so it belongs on a worker.
pub(super) fn stage_animation(spec: &BigImageSpec) -> Result<StagedRequest, BigImageError> {
    stage_request(spec)
}

/// Stage a source for a persistent region decoder (blocking; run on a worker).
pub(super) fn stage_region(spec: &BigImageSpec) -> Result<StagedRequest, BigImageError> {
    stage_request(spec)
}

/// A live sandboxed decoder for one large still image, reused for many region
/// crops.
///
/// Opening reads and stages the source once and hands it to the sandbox, where
/// the reduce-decoding JPEG loader keeps it in memory. Each region is then a
/// cheap `specific_frame` clip from that in-memory data, so panning a gigapixel
/// costs a small crop rather than re-reading a 100 MB file every move. The
/// decoder is held only while a deep zoom needs it; dropping it returns the
/// sandbox process to glycin's pool.
pub(super) struct SandboxedRegionDecoder {
    image: glycin::Image,
    natural: (u32, u32),
    _staged: StagedSource,
}

impl SandboxedRegionDecoder {
    /// Open a persistent region decoder, or `None` when region decode does not
    /// apply (only the reduce-decoding JPEG loader honors a clip; any other
    /// loader would return the whole image scaled and mislabel it as the region).
    pub(super) async fn open(
        spec: StagedRequest,
        mechanism: glycin::SandboxMechanism,
    ) -> Result<Option<Self>, BigImageError> {
        let mut loader = staged_loader(&spec);
        loader.sandbox_selector(mechanism.into_selector());
        loader.accepted_memory_formats(glycin::MemoryFormatSelection::R8g8b8a8);
        let image = loader
            .load()
            .await
            .map_err(|_| BigImageError::SandboxedDecodeFailed)?;
        if image.active_sandbox_mechanism() == glycin::SandboxMechanism::NotSandboxed {
            return Err(BigImageError::SandboxUnavailable);
        }
        if !(image.mime_type().as_str() == "image/jpeg" && crate::large_jpeg_source_enabled()) {
            return Ok(None);
        }
        // A clip is in the source's stored pixels, but glycin turns the delivered
        // image to its EXIF orientation. On a rotated or mirrored image the two
        // disagree and a tile would land in the wrong place, so region decode is
        // declined and the caller keeps the (correctly turned) base frame.
        let orientation = image.transformation_orientation();
        if orientation.rotate().degrees() != 0 || orientation.mirror() {
            return Ok(None);
        }
        let details = image.details();
        validate_dimensions(
            details.width(),
            details.height(),
            LARGE_JPEG_MAX_SOURCE_PIXELS,
        )?;
        let natural = (details.width(), details.height());
        Ok(Some(Self {
            image,
            natural,
            _staged: spec.source,
        }))
    }

    pub(super) fn natural(&self) -> (u32, u32) {
        self.natural
    }

    /// Decode source rectangle `clip` (natural pixels) at output size `target`.
    pub(super) async fn decode_region(
        &self,
        clip: (u32, u32, u32, u32),
        target: (i32, i32),
    ) -> Result<BigRgbaImage, BigImageError> {
        let clip = clamp_clip_to_natural(
            clip,
            (
                i32::try_from(self.natural.0).unwrap_or(i32::MAX),
                i32::try_from(self.natural.1).unwrap_or(i32::MAX),
            ),
        );
        let target_w = target.0.clamp(1, clip.2);
        let target_h = target.1.clamp(1, clip.3);
        let frame = self
            .image
            .specific_frame(
                glycin::FrameRequest::new()
                    .scale(target_w as u32, target_h as u32)
                    .clip(clip.0 as u32, clip.1 as u32, clip.2 as u32, clip.3 as u32),
            )
            .await
            .map_err(|_| BigImageError::SandboxedDecodeFailed)?;
        validate_display_dimensions(frame.width(), frame.height())?;
        let frame_width =
            i32::try_from(frame.width()).map_err(|_| BigImageError::InvalidBytePixels)?;
        let frame_height =
            i32::try_from(frame.height()).map_err(|_| BigImageError::InvalidBytePixels)?;
        let stride =
            usize::try_from(frame.stride()).map_err(|_| BigImageError::InvalidBytePixels)?;
        let mut image = pack_rows_to_rgba(frame.buf_slice(), frame_width, frame_height, stride)
            .ok_or(BigImageError::InvalidBytePixels)?;
        image.cicp = super::frame_cicp(&frame);
        Ok(image)
    }
}

/// Where the full-size frame a loader may deliver is charged while it is
/// scaled down to the display bound.
#[derive(Clone, Copy)]
pub(super) struct WholeFrameAdmission<'a> {
    pub(super) resources: &'a Arc<SessionResources>,
    pub(super) class: MemoryClass,
    /// Bytes the caller already holds for the display frame of this decode.
    pub(super) held: usize,
}

impl WholeFrameAdmission<'_> {
    /// Four bytes a pixel covers every format the loader may hand over. A
    /// frame larger than the governor takes all that the caller's own display
    /// frame leaves, so it opens only when nothing else is held; the
    /// source-pixel cap bounds what it can be.
    fn reserve(self, natural: (i32, i32)) -> Result<MemoryLease, BigImageError> {
        let bytes =
            crate::admission::rgba_capacity(natural).ok_or(BigImageError::InvalidDisplayBound)?;
        let total = self.resources.snapshot().limits.bytes[MemoryDomain::Cpu as usize];
        let bytes = bytes.min(total.saturating_sub(self.held));
        self.resources
            .reserve(MemoryDomain::Cpu, self.class, bytes)
            .ok_or(BigImageError::ResourceLimit)
    }
}

async fn decode_sandboxed(
    loader: glycin::Loader,
    bound: (i32, i32),
    clip: Option<(u32, u32, u32, u32)>,
    max_pixels: u64,
    mechanism: glycin::SandboxMechanism,
    cancellable: Option<&gio::Cancellable>,
    whole_frame: Option<WholeFrameAdmission<'_>>,
) -> Result<(BigRgbaImage, super::BigSourceSize), BigImageError> {
    let (frame, (target_width, target_height), source, held) = load_frame_sandboxed(
        loader,
        bound,
        clip,
        max_pixels,
        BigImageFit::Contain,
        mechanism,
        cancellable,
        whole_frame,
    )
    .await?;
    // Off the main context: when glycin ignores the size asked for, packing is
    // a full-resolution scale.
    let image = off_main_context(move || {
        let image = frame_to_rgba(&frame, target_width, target_height);
        // The full frame is released with its charge, never before.
        drop(frame);
        drop(held);
        image
    })
    .await??;
    Ok((image, source))
}

/// Pack a delivered glycin frame into a [`BigRgbaImage`], honoring the target
/// bound, the frame's alpha, and its color space.
fn frame_to_rgba(
    frame: &glycin::Frame,
    target_width: i32,
    target_height: i32,
) -> Result<BigRgbaImage, BigImageError> {
    let frame_width = i32::try_from(frame.width()).map_err(|_| BigImageError::InvalidBytePixels)?;
    let frame_height =
        i32::try_from(frame.height()).map_err(|_| BigImageError::InvalidBytePixels)?;
    // Glycin's frame scale is only a hint: glycin 2.1, still shipped on older
    // systems, returns the image at full resolution however small the request.
    // When it does, scale the frame down in one step (wrapping its bytes as a
    // pixbuf and scaling) instead of packing the whole frame and then resizing
    // it, which copies the full buffer twice. Newer glycin returns the bound,
    // so the packing fast path runs.
    if frame_width > target_width || frame_height > target_height {
        return frame_to_rgba_scaled(frame, target_width, target_height);
    }
    let stride = usize::try_from(frame.stride()).map_err(|_| BigImageError::InvalidBytePixels)?;
    let opaque = frame.memory_format() == glycin::MemoryFormat::R8g8b8;
    let mut image = if opaque {
        pack_rgb_rows_to_rgbx(frame.buf_slice(), frame_width, frame_height, stride)
    } else {
        pack_rows_to_rgba(frame.buf_slice(), frame_width, frame_height, stride)
    }
    .ok_or(BigImageError::InvalidBytePixels)?;
    // The sandboxed path is the one every viewer image takes, so this is where
    // the color space has to be carried out of the sandbox with the pixels.
    image.cicp = super::frame_cicp(frame);
    Ok(image)
}

/// Scale an oversized glycin frame straight into packed RGBA.
///
/// The frame bytes become the backing store of a `Pixbuf` at their real stride
/// (one copy, no repack), which is scaled to the target; only the small result
/// is then packed. Packing the full frame and then resizing the packed copy
/// would hold the full-resolution buffer twice. The frame's color state
/// is carried onto the result so wide-gamut sources are not silently shown sRGB.
fn frame_to_rgba_scaled(
    frame: &glycin::Frame,
    target_width: i32,
    target_height: i32,
) -> Result<BigRgbaImage, BigImageError> {
    let frame_width = i32::try_from(frame.width()).map_err(|_| BigImageError::InvalidBytePixels)?;
    let frame_height =
        i32::try_from(frame.height()).map_err(|_| BigImageError::InvalidBytePixels)?;
    let stride = i32::try_from(frame.stride()).map_err(|_| BigImageError::InvalidBytePixels)?;
    // Zero-copy: `buf_bytes` hands out the frame's memfd-backed `glib::Bytes`, so
    // the pixbuf reads the sandbox mapping directly instead of copying the whole
    // full-resolution frame onto our heap before scaling it down.
    let bytes = frame.buf_bytes();
    // The frame may be RGB (opaque source) or RGBA; the pixbuf must be told which
    // so its rowstride matches. `pixbuf_to_rgba` widens RGB to RGBA afterwards.
    let has_alpha = frame.memory_format() == glycin::MemoryFormat::R8g8b8a8;
    let source = gtk::gdk_pixbuf::Pixbuf::from_bytes(
        &bytes,
        gtk::gdk_pixbuf::Colorspace::Rgb,
        has_alpha,
        8,
        frame_width,
        frame_height,
        stride,
    );
    let scaled = source
        .scale_simple(
            target_width,
            target_height,
            gtk::gdk_pixbuf::InterpType::Bilinear,
        )
        .ok_or(BigImageError::InvalidBytePixels)?;
    let mut image = super::pixbuf_to_rgba(&scaled).ok_or(BigImageError::InvalidBytePixels)?;
    image.cicp = super::frame_cicp(frame);
    Ok(image)
}

async fn load_frame_sandboxed(
    mut loader: glycin::Loader,
    bound: (i32, i32),
    clip: Option<(u32, u32, u32, u32)>,
    max_pixels: u64,
    fit: BigImageFit,
    mechanism: glycin::SandboxMechanism,
    cancellable: Option<&gio::Cancellable>,
    whole_frame: Option<WholeFrameAdmission<'_>>,
) -> Result<
    (
        glycin::Frame,
        (i32, i32),
        super::BigSourceSize,
        Option<MemoryLease>,
    ),
    BigImageError,
> {
    loader.sandbox_selector(mechanism.into_selector());
    // RGB is accepted beside RGBA so an opaque source is not widened to a
    // fourth, always-opaque channel before it is packed or reaches the GPU.
    loader.accepted_memory_formats(
        glycin::MemoryFormatSelection::R8g8b8 | glycin::MemoryFormatSelection::R8g8b8a8,
    );
    let image = loader.load().await.map_err(|_| {
        if cancellable.is_some_and(gio::prelude::CancellableExt::is_cancelled) {
            BigImageError::Cancelled
        } else {
            BigImageError::SandboxedDecodeFailed
        }
    })?;
    if image.active_sandbox_mechanism() == glycin::SandboxMechanism::NotSandboxed {
        return Err(BigImageError::SandboxUnavailable);
    }

    let details = image.details();
    let mime = image.mime_type();
    let is_scalable = matches!(mime.as_str(), "image/svg+xml" | "image/svg+xml-compressed");
    // A reduce-decoding JPEG loader (glycin-jpegturbo, enabled by the app)
    // delivers a small frame from a huge source, so the source-pixel cap that
    // guards against full-resolution decodes can be lifted for JPEG then. The
    // delivered frame is then held to `max_pixels` below.
    let large_jpeg = mime.as_str() == "image/jpeg" && crate::large_jpeg_source_enabled();
    let source_cap = if large_jpeg {
        LARGE_JPEG_MAX_SOURCE_PIXELS
    } else {
        MAX_SOURCE_PIXELS
    };
    validate_dimensions(details.width(), details.height(), source_cap)?;
    let natural = (
        i32::try_from(details.width()).map_err(|_| BigImageError::PixelLimitExceeded {
            width: details.width(),
            height: details.height(),
            max_pixels: MAX_SOURCE_PIXELS,
        })?,
        i32::try_from(details.height()).map_err(|_| BigImageError::PixelLimitExceeded {
            width: details.width(),
            height: details.height(),
            max_pixels: MAX_SOURCE_PIXELS,
        })?,
    );
    // A clip is only honored by our reduce-decoding JPEG loader; any other
    // loader would return the whole image scaled and pass it off as the region.
    // Refuse rather than mislead, so the caller keeps the frame it already shows.
    let clip = match clip {
        Some(clip) if large_jpeg => Some(clamp_clip_to_natural(clip, natural)),
        Some(_) => return Err(BigImageError::SandboxedDecodeFailed),
        None => None,
    };
    // The output size is fit against the region when clipping (the region is what
    // is rendered), else against the whole image.
    let fit_natural = clip.map_or(natural, |(_, _, w, h)| (w, h));
    let (width, height) = effective_texture_size(bound, fit_natural, is_scalable, fit)
        .ok_or(BigImageError::InvalidDisplayBound)?;
    validate_dimensions(
        u32::try_from(width).map_err(|_| BigImageError::InvalidDisplayBound)?,
        u32::try_from(height).map_err(|_| BigImageError::InvalidDisplayBound)?,
        max_pixels,
    )?;
    // A loader that ignores the size asked for delivers the whole frame, and
    // that frame is alive until it is scaled down: charge it for that time.
    // Ours reduces JPEG and PNG while decoding.
    let reduced =
        large_jpeg || (mime.as_str() == "image/png" && crate::jpeg_loader::reduced_png_source());
    let held = match whole_frame {
        Some(admission)
            if !is_scalable && !reduced && (natural.0 > width || natural.1 > height) =>
        {
            Some(admission.reserve(natural)?)
        }
        _ => None,
    };
    let mut request = glycin::FrameRequest::new().scale(
        u32::try_from(width).map_err(|_| BigImageError::InvalidDisplayBound)?,
        u32::try_from(height).map_err(|_| BigImageError::InvalidDisplayBound)?,
    );
    if let Some((x, y, w, h)) = clip {
        request = request.clip(
            u32::try_from(x).map_err(|_| BigImageError::InvalidDisplayBound)?,
            u32::try_from(y).map_err(|_| BigImageError::InvalidDisplayBound)?,
            u32::try_from(w).map_err(|_| BigImageError::InvalidDisplayBound)?,
            u32::try_from(h).map_err(|_| BigImageError::InvalidDisplayBound)?,
        );
    }
    let frame = image.specific_frame(request).await.map_err(|_| {
        if cancellable.is_some_and(gio::prelude::CancellableExt::is_cancelled) {
            BigImageError::Cancelled
        } else {
            BigImageError::SandboxedDecodeFailed
        }
    })?;
    // Glycin's scale is a hint some loaders ignore (image-rs delivers a PNG
    // whole), and every caller scales a larger frame down to the target held
    // to `max_pixels` above, off the main context. A source within
    // MAX_SOURCE_PIXELS may come back whole; the lifted JPEG cap relies on the
    // reduce-decoding loader honoring the size asked.
    let frame_cap = if source_cap == MAX_SOURCE_PIXELS {
        max_pixels.max(u64::from(details.width()) * u64::from(details.height()))
    } else {
        max_pixels
    };
    validate_dimensions(frame.width(), frame.height(), frame_cap)?;
    // `width`/`height` are the fit-aware target the loader was asked for; callers
    // compare the delivered frame against it to decide whether a downscale is
    // still owed (old glycin ignores the request and returns full resolution).
    // Glycin has already swapped the details' sides for an EXIF quarter turn.
    let source = super::BigSourceSize {
        natural,
        scalable: is_scalable,
        // Glycin converts from an ICC profile unless CICP describes the colors.
        color_profile: match super::frame_cicp(&frame) {
            Some(cicp) => (cicp.color_primaries, cicp.transfer_characteristics) != (1, 13),
            None => frame
                .details()
                .color_icc_profile()
                .is_some_and(|icc| icc.get_full().is_ok_and(|icc| !super::names_srgb(&icc))),
        },
    };
    Ok((frame, (width, height), source, held))
}

/// Clamp a requested clip to the image, in natural pixels.
///
/// A region that starts past an edge, or overruns it, is pulled back inside so
/// the loader never sees a rectangle outside the image. Width and height stay at
/// least one pixel.
fn clamp_clip_to_natural(clip: (u32, u32, u32, u32), natural: (i32, i32)) -> (i32, i32, i32, i32) {
    let (nw, nh) = natural;
    let x = i64::from(clip.0).clamp(0, i64::from(nw).saturating_sub(1)) as i32;
    let y = i64::from(clip.1).clamp(0, i64::from(nh).saturating_sub(1)) as i32;
    let w = i64::from(clip.2).clamp(1, i64::from(nw - x)) as i32;
    let h = i64::from(clip.3).clamp(1, i64::from(nh - y)) as i32;
    (x, y, w, h)
}

fn effective_texture_size(
    bound: (i32, i32),
    natural: (i32, i32),
    scalable: bool,
    fit: BigImageFit,
) -> Option<(i32, i32)> {
    // The bound was validated when the request was staged.
    let (bw, bh) = bound;
    positive_pixel_count(bw, bh)?;
    let (nw, nh) = natural;
    if nw <= 0 || nh <= 0 {
        return None;
    }

    match fit {
        BigImageFit::Contain | BigImageFit::ScaleDown => {
            super::effective_decode_bound(Some((bw, bh)), Some((nw, nh)), scalable)
        }
        BigImageFit::Fill => {
            if scalable {
                return Some((bw, bh));
            }
            // Decoding cannot add information. If the source is smaller in a
            // dimension, keep that dimension natural and let GDK perform the
            // unavoidable upscale at presentation time.
            Some((bw.min(nw).max(1), bh.min(nh).max(1)))
        }
        BigImageFit::Cover => {
            let scale = f64::max(f64::from(bw) / f64::from(nw), f64::from(bh) / f64::from(nh));
            if !scalable && scale >= 1.0 {
                return Some((nw, nh));
            }
            let width = (f64::from(nw) * scale).ceil();
            let height = (f64::from(nh) * scale).ceil();
            if !(1.0..=f64::from(i32::MAX)).contains(&width)
                || !(1.0..=f64::from(i32::MAX)).contains(&height)
            {
                return None;
            }
            Some((width as i32, height as i32))
        }
    }
}

#[cfg(test)]
mod tests;
