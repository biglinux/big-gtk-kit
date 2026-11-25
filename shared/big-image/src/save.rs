// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Writing a decoded frame to a file, in the format its extension names, out of
//! what gdk-pixbuf on this system can write. JPEG and PNG carry the source's
//! EXIF over; the pixels are written in sRGB, as glycin converted them from any
//! colour profile and as pixels in another colour space are converted here.
//! Nothing here writes over a file the user did not name.

use big_os_kit::crc32;
use std::path::Path;
use std::sync::{Condvar, Mutex, PoisonError};

use super::{BigImageError, BigRgbaImage};

/// A format this build can write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BigImageSaveFormat {
    /// Lossless, keeps alpha.
    Png,
    /// Lossy, drops alpha.
    Jpeg,
    /// Lossy, keeps alpha, much smaller than PNG.
    Webp,
    /// Lossless, keeps alpha, what scanners and printers ask for.
    Tiff,
}

/// Every format this crate knows how to write, in the order a chooser lists
/// them. Whether the running system can actually write one is a separate
/// question, answered by [`BigImageSaveFormat::is_available`].
pub const BIG_IMAGE_SAVE_FORMATS: [BigImageSaveFormat; 4] = [
    BigImageSaveFormat::Png,
    BigImageSaveFormat::Jpeg,
    BigImageSaveFormat::Webp,
    BigImageSaveFormat::Tiff,
];

impl BigImageSaveFormat {
    /// The format `path`'s extension asks for, when it is one we can write.
    #[must_use]
    pub fn for_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        BIG_IMAGE_SAVE_FORMATS
            .into_iter()
            .find(|format| format.extensions().contains(&extension.as_str()))
    }

    /// The extensions that name this format, the canonical one first.
    #[must_use]
    pub const fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Png => &["png"],
            Self::Jpeg => &["jpg", "jpeg"],
            Self::Webp => &["webp"],
            Self::Tiff => &["tiff", "tif"],
        }
    }

    /// The content type of a file in this format.
    #[must_use]
    pub const fn mime_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Tiff => "image/tiff",
        }
    }

    /// The gdk-pixbuf writer name.
    const fn pixbuf_type(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
            Self::Tiff => "tiff",
        }
    }

    /// Whether a copy in this format keeps the source's camera data (EXIF).
    #[must_use]
    pub const fn keeps_camera_data(self) -> bool {
        matches!(self, Self::Jpeg | Self::Png)
    }

    /// Whether a copy in this format keeps the camera data in `carried`.
    #[must_use]
    pub fn keeps_camera_data_of(self, carried: &BigCarriedMetadata) -> bool {
        self.camera_data(carried).is_some()
    }

    /// The EXIF a copy in this format carries from `carried`: upright, and
    /// only when its directory reads and it fits the container.
    fn camera_data(self, carried: &BigCarriedMetadata) -> Option<Vec<u8>> {
        if !self.keeps_camera_data() {
            return None;
        }
        let exif = crate::exif::upright(carried.exif.as_deref()?)?;
        (self != Self::Jpeg || exif.len() <= MAX_JPEG_EXIF).then_some(exif)
    }

    /// Whether this container carries an alpha channel at all.
    #[must_use]
    pub(crate) const fn keeps_alpha(self) -> bool {
        !matches!(self, Self::Jpeg)
    }

    /// Whether the running system has a writer for this format.
    ///
    /// PNG and JPEG ship with gdk-pixbuf itself; WebP and TIFF arrive with
    /// separate packages that a given install may not have. Asked of the
    /// library rather than assumed, so a chooser never offers a format that
    /// fails only after the person has named the file.
    #[must_use]
    pub fn is_available(self) -> bool {
        gtk::gdk_pixbuf::Pixbuf::formats().iter().any(|format| {
            format.is_writable() && format.name().as_deref() == Some(self.pixbuf_type())
        })
    }
}

/// Bytes of memory a save holds at its peak for each pixel: the sandboxed
/// loader's frame, the frame it hands over and its packed copy, a turned or
/// resized copy, and the encoder's own.
const SAVE_BYTES_PER_PIXEL: u64 = 20;

/// Whether a save holds its turn in this process.
static SAVING: Mutex<bool> = Mutex::new(false);
static SAVE_ENDED: Condvar = Condvar::new();

/// The turn of one save, held until it is dropped. Saves run one at a time,
/// so the free memory each one measures is not also claimed by another.
#[must_use]
#[derive(Debug)]
pub struct BigSaveTurn {
    ceiling: u64,
}

impl BigSaveTurn {
    /// Wait until no other save in this process holds its turn, then
    /// measure the memory this one may use. Blocking: it belongs on a worker.
    pub fn wait() -> Self {
        let mut saving = SAVING.lock().unwrap_or_else(PoisonError::into_inner);
        while *saving {
            saving = SAVE_ENDED
                .wait(saving)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *saving = true;
        drop(saving);
        Self {
            ceiling: save_pixel_ceiling(),
        }
    }

    /// The most pixels this save may decode.
    #[must_use]
    pub const fn pixel_ceiling(&self) -> u64 {
        self.ceiling
    }

    /// Refuse a picture of `size` this save cannot hold.
    ///
    /// # Errors
    ///
    /// [`BigImageError::PixelLimitExceeded`] past the ceiling on any source,
    /// [`BigImageError::ResourceLimit`] past what the free memory holds.
    pub fn admit(&self, (width, height): (i32, i32)) -> Result<(), BigImageError> {
        let (width, height) = (width.unsigned_abs(), height.unsigned_abs());
        let pixels = u64::from(width) * u64::from(height);
        if pixels > crate::secure::MAX_SOURCE_PIXELS {
            return Err(BigImageError::PixelLimitExceeded {
                width,
                height,
                max_pixels: crate::secure::MAX_SOURCE_PIXELS,
            });
        }
        if pixels > self.ceiling {
            return Err(BigImageError::ResourceLimit);
        }
        Ok(())
    }
}

impl Drop for BigSaveTurn {
    fn drop(&mut self) {
        *SAVING.lock().unwrap_or_else(PoisonError::into_inner) = false;
        SAVE_ENDED.notify_one();
    }
}

/// The most pixels a save may decode: as many as the memory this process can
/// still allocate holds, up to the decoder's ceiling on a source; unknown, the
/// display ceiling every decode already fits. What a resize offers; a save
/// measures it again under its [`BigSaveTurn`]. Blocking: it reads `/proc`
/// and the cgroup files.
#[must_use]
pub fn save_pixel_ceiling() -> u64 {
    big_os_kit::system_memory::allocatable_bytes()
        .map_or(crate::secure::MAX_DISPLAY_PIXELS, |bytes| {
            (bytes / SAVE_BYTES_PER_PIXEL).min(crate::secure::MAX_SOURCE_PIXELS)
        })
}

/// What a copy carries over from the picture it was made from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BigCarriedMetadata {
    /// The source's EXIF: a TIFF header and its directories.
    pub exif: Option<Vec<u8>>,
    /// The quality the source JPEG was written at.
    pub jpeg_quality: Option<u8>,
}

/// Encode `image` and write it to `path`, replacing it atomically.
///
/// Runs on the calling thread; it belongs on a worker.
///
/// # Errors
///
/// As [`save_rgba_carrying`].
pub fn save_rgba(image: &BigRgbaImage, path: &Path) -> Result<(), BigImageError> {
    save_rgba_carrying(image, path, &BigCarriedMetadata::default())
}

/// [`save_rgba`] for a copy of a picture: its EXIF goes along where the format
/// holds it, Orientation reset as the pixels stand upright, and a JPEG is
/// written at `carried.jpeg_quality` when it is known.
///
/// Runs on the calling thread; it belongs on a worker.
///
/// # Errors
///
/// [`BigImageError::UnsupportedSaveFormat`] for an extension no encoder writes,
/// the errors of [`encode_rgba`], and [`BigImageError::SaveFailed`] when the
/// file is not written.
pub fn save_rgba_carrying(
    image: &BigRgbaImage,
    path: &Path,
    carried: &BigCarriedMetadata,
) -> Result<(), BigImageError> {
    let format = BigImageSaveFormat::for_path(path).ok_or(BigImageError::UnsupportedSaveFormat)?;
    let mut bytes = encode(image, format, carried.jpeg_quality.unwrap_or(JPEG_QUALITY))?;
    if let Some(exif) = format.camera_data(carried) {
        bytes = with_exif(bytes, format, &exif);
    }
    big_os_kit::storage::atomic_save_document(path, &bytes).map_err(|error| {
        BigImageError::SaveFailed {
            reason: big_os_kit::file_io::system_words(&error),
        }
    })
}

/// The most EXIF one JPEG APP1 segment holds after its length and marker.
const MAX_JPEG_EXIF: usize = u16::MAX as usize - 2 - 6;

/// The quality of a JPEG with no source quality to keep: gdk-pixbuf's own 75
/// shows blocks around text and edges, which is most of what a screenshot is.
const JPEG_QUALITY: u8 = 92;

/// `image` encoded as `format`, for a caller that writes or copies the bytes.
///
/// Runs on the calling thread; it belongs on a worker.
///
/// # Errors
///
/// [`BigImageError::InvalidBytePixels`] when the buffer does not match its size,
/// and [`BigImageError::SaveFailed`] when the encoder refuses it.
pub fn encode_rgba(
    image: &BigRgbaImage,
    format: BigImageSaveFormat,
) -> Result<Vec<u8>, BigImageError> {
    encode(image, format, JPEG_QUALITY)
}

impl BigRgbaImage {
    /// These pixels in sRGB, which is how a file that names no colour space
    /// is read. Pixels already in sRGB come back as they are. Blocking.
    ///
    /// # Errors
    ///
    /// [`BigImageError::InvalidBytePixels`] when the buffer does not match
    /// its size.
    pub fn into_srgb(self) -> Result<Self, BigImageError> {
        if self.cicp.is_none() {
            return Ok(self);
        }
        let stride = crate::texture_stride(&self).ok_or(BigImageError::InvalidBytePixels)?;
        let shape = Self {
            rgba: Vec::new(),
            ..self
        };
        let texture =
            crate::texture_from_bytes(&gtk::glib::Bytes::from_owned(self.rgba), &shape, stride);
        let mut downloader = gtk::gdk::TextureDownloader::new(&texture);
        downloader.set_format(gtk::gdk::MemoryFormat::R8g8b8a8);
        downloader.set_color_state(&gtk::gdk::ColorState::srgb());
        let (bytes, downloaded_stride) = downloader.download_bytes();
        drop(texture);
        let height = usize::try_from(shape.height).map_err(|_| BigImageError::InvalidBytePixels)?;
        let mut rgba = Vec::with_capacity(stride * height);
        for row in bytes.chunks(downloaded_stride).take(height) {
            rgba.extend_from_slice(row.get(..stride).ok_or(BigImageError::InvalidBytePixels)?);
        }
        if rgba.len() != stride * height {
            return Err(BigImageError::InvalidBytePixels);
        }
        Ok(Self {
            rgba,
            cicp: None,
            ..shape
        })
    }
}

/// `image` as it is when in sRGB, else a converted copy.
pub(crate) fn in_srgb(
    image: &BigRgbaImage,
) -> Result<std::borrow::Cow<'_, BigRgbaImage>, BigImageError> {
    if image.cicp.is_none() {
        return Ok(std::borrow::Cow::Borrowed(image));
    }
    image.clone().into_srgb().map(std::borrow::Cow::Owned)
}

fn encode(
    image: &BigRgbaImage,
    format: BigImageSaveFormat,
    jpeg_quality: u8,
) -> Result<Vec<u8>, BigImageError> {
    let image = &in_srgb(image)?;
    // A JPEG has no alpha channel, and the encoder says so by refusing the
    // buffer rather than by flattening it: every save as .jpg failed with
    // "does not support the color type Rgba8" until the flattening happened
    // here, where the colour to show through is a decision and not a default.
    // An opaque picture is written without the alpha channel too: every byte
    // of it would be 255, and a PNG carrying them is a third larger.
    let pixbuf = if format.keeps_alpha() && !image.opaque {
        rgba_to_pixbuf(image)
    } else {
        rgb_over_white_pixbuf(image)
    }
    .ok_or(BigImageError::InvalidBytePixels)?;
    let quality = jpeg_quality.clamp(1, 100).to_string();
    let options: &[(&str, &str)] = match format {
        BigImageSaveFormat::Jpeg => &[("quality", &quality)],
        _ => &[],
    };
    pixbuf
        .save_to_bufferv(format.pixbuf_type(), options)
        .map_err(|error| BigImageError::SaveFailed {
            reason: error.to_string(),
        })
}

/// `encoded` with `exif` (a TIFF header and its directories) in it: an APP1
/// segment after a JPEG's start and JFIF header, an `eXIf` chunk after a PNG's header. An EXIF
/// too long for one JPEG segment, or bytes not shaped as gdk-pixbuf writes
/// them, are left as they were.
fn with_exif(encoded: Vec<u8>, format: BigImageSaveFormat, exif: &[u8]) -> Vec<u8> {
    let (at, inserted) = match format {
        BigImageSaveFormat::Jpeg => {
            let Ok(length) = u16::try_from(2 + 6 + exif.len()) else {
                return encoded;
            };
            if !encoded.starts_with(&[0xFF, 0xD8]) {
                return encoded;
            }
            let mut segment = vec![0xFF, 0xE1];
            segment.extend_from_slice(&length.to_be_bytes());
            segment.extend_from_slice(b"Exif\0\0");
            segment.extend_from_slice(exif);
            // JFIF asks for its APP0 straight after the start.
            let after_jfif = match encoded.get(2..6) {
                Some(&[0xFF, 0xE0, high, low]) => 4 + usize::from(u16::from_be_bytes([high, low])),
                _ => 2,
            };
            if after_jfif > encoded.len() {
                return encoded;
            }
            (after_jfif, segment)
        }
        BigImageSaveFormat::Png => {
            // Signature, then IHDR: length, type, 13 bytes, CRC.
            let Ok(length) = u32::try_from(exif.len()) else {
                return encoded;
            };
            if encoded.get(12..16) != Some(b"IHDR") {
                return encoded;
            }
            let mut chunk = length.to_be_bytes().to_vec();
            chunk.extend_from_slice(b"eXIf");
            chunk.extend_from_slice(exif);
            let crc = crc32::of(&chunk[4..]);
            chunk.extend_from_slice(&crc.to_be_bytes());
            (8 + 25, chunk)
        }
        BigImageSaveFormat::Webp | BigImageSaveFormat::Tiff => return encoded,
    };
    let mut out = Vec::with_capacity(encoded.len() + inserted.len());
    out.extend_from_slice(&encoded[..at]);
    out.extend_from_slice(&inserted);
    out.extend_from_slice(&encoded[at..]);
    out
}

/// Encode `image` as PNG bytes, for a caller that writes them itself.
///
/// Runs on the calling thread; it belongs on a worker.
#[must_use]
pub fn png_bytes(image: &BigRgbaImage) -> Option<Vec<u8>> {
    rgba_to_pixbuf(&*in_srgb(image).ok()?)?
        .save_to_bufferv("png", &[])
        .ok()
}

/// Wrap packed RGBA in a `Pixbuf` that owns its own copy of the bytes.
pub(crate) fn rgba_to_pixbuf(image: &BigRgbaImage) -> Option<gtk::gdk_pixbuf::Pixbuf> {
    let width = usize::try_from(image.width).ok()?;
    let rowstride = width.checked_mul(4)?;
    let expected = rowstride.checked_mul(usize::try_from(image.height).ok()?)?;
    if image.width <= 0 || image.height <= 0 || image.rgba.len() != expected {
        return None;
    }
    Some(gtk::gdk_pixbuf::Pixbuf::from_bytes(
        &gtk::glib::Bytes::from(&image.rgba),
        gtk::gdk_pixbuf::Colorspace::Rgb,
        true,
        8,
        image.width,
        image.height,
        i32::try_from(rowstride).ok()?,
    ))
}

/// The same pixels with the transparency laid over white, three bytes each.
///
/// White because that is what a transparent picture looks like on every page,
/// window and sheet of paper it will end up on; leaving the raw colour behind
/// turns an invisible logo into a solid black rectangle.
fn rgb_over_white_pixbuf(image: &BigRgbaImage) -> Option<gtk::gdk_pixbuf::Pixbuf> {
    let width = usize::try_from(image.width).ok()?;
    let height = usize::try_from(image.height).ok()?;
    let expected = width.checked_mul(height)?.checked_mul(4)?;
    if image.width <= 0 || image.height <= 0 || image.rgba.len() != expected {
        return None;
    }
    let mut rgb = Vec::new();
    rgb.try_reserve_exact(width * height * 3).ok()?;
    for pixel in image.rgba.as_chunks::<4>().0 {
        let alpha = u32::from(pixel[3]);
        for channel in &pixel[..3] {
            let blended = (u32::from(*channel) * alpha + 255 * (255 - alpha) + 127) / 255;
            rgb.push(blended.min(255) as u8);
        }
    }
    Some(gtk::gdk_pixbuf::Pixbuf::from_bytes(
        &gtk::glib::Bytes::from(&rgb),
        gtk::gdk_pixbuf::Colorspace::Rgb,
        false,
        8,
        image.width,
        image.height,
        i32::try_from(width.checked_mul(3)?).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fully transparent picture saved as JPEG is white, not black.
    #[test]
    fn alpha_is_flattened_over_white() {
        let transparent = BigRgbaImage {
            rgba: vec![0; 4 * 4 * 4],
            width: 4,
            height: 4,
            cicp: None,
            opaque: false,
        };
        let pixbuf = rgb_over_white_pixbuf(&transparent).expect("well-formed buffer");
        assert!(!pixbuf.has_alpha());
        // SAFETY: reading the bytes of a pixbuf this test just built and owns.
        let bytes = unsafe { pixbuf.pixels() };
        assert!(
            bytes
                .as_chunks::<3>()
                .0
                .iter()
                .take(16)
                .all(|pixel| *pixel == [255; 3]),
            "nothing was there, so what shows through is the page"
        );
    }

    /// An opaque picture is written as RGB: alpha bytes that are all 255 only
    /// make the file larger.
    #[test]
    fn opaque_picture_has_no_alpha() {
        let opaque = BigRgbaImage {
            rgba: [10, 20, 30, 255].repeat(16),
            width: 4,
            height: 4,
            cicp: None,
            opaque: true,
        };
        let png = encode_rgba(&opaque, BigImageSaveFormat::Png).expect("encoded");
        use gtk::gdk_pixbuf::prelude::PixbufLoaderExt;
        #[expect(
            clippy::disallowed_methods,
            reason = "reads back the PNG the test encoded"
        )]
        let loader = gtk::gdk_pixbuf::PixbufLoader::new();
        loader.write(&png).expect("a PNG");
        loader.close().expect("a whole PNG");
        let decoded = loader.pixbuf().expect("decoded");
        assert!(!decoded.has_alpha());
        // SAFETY: reading the bytes of a pixbuf this test decoded and owns.
        assert_eq!(&unsafe { decoded.pixels() }[..3], &[10, 20, 30]);
    }

    #[test]
    fn refused_save_keeps_system_words() {
        let image = BigRgbaImage {
            rgba: vec![255; 4],
            width: 1,
            height: 1,
            cicp: None,
            opaque: true,
        };
        let missing = std::env::temp_dir()
            .join(format!("big-image-missing-{}", std::process::id()))
            .join("a.png");
        let Err(BigImageError::SaveFailed { reason }) = save_rgba(&image, &missing) else {
            panic!("a folder that does not exist refuses the write");
        };
        assert!(
            !reason.is_empty() && !reason.contains("os error"),
            "{reason}"
        );
    }

    #[test]
    fn unfit_exif_counts_as_lost() {
        let mut tiff = b"MM\0*\0\0\0\x08\0\0\0\0\0\0".to_vec();
        tiff.resize(70_000, 0);
        let long = BigCarriedMetadata {
            exif: Some(tiff),
            jpeg_quality: None,
        };
        assert!(
            !BigImageSaveFormat::Jpeg.keeps_camera_data_of(&long),
            "past one APP1 segment"
        );
        assert!(BigImageSaveFormat::Png.keeps_camera_data_of(&long));
        let unreadable = BigCarriedMetadata {
            exif: Some(b"MM\0*\0\0\0\x40".to_vec()),
            jpeg_quality: None,
        };
        assert!(!BigImageSaveFormat::Png.keeps_camera_data_of(&unreadable));
    }

    #[test]
    fn exif_follows_jfif_header() {
        let image = BigRgbaImage {
            rgba: vec![255; 4],
            width: 1,
            height: 1,
            cicp: None,
            opaque: true,
        };
        let encoded = encode_rgba(&image, BigImageSaveFormat::Jpeg).expect("encoded");
        assert_eq!(
            &encoded[2..4],
            &[0xFF, 0xE0],
            "gdk-pixbuf writes JFIF first"
        );
        let app0_end = 4 + usize::from(u16::from_be_bytes([encoded[4], encoded[5]]));
        let with = with_exif(encoded, BigImageSaveFormat::Jpeg, b"MM\0*");
        assert_eq!(&with[2..4], &[0xFF, 0xE0]);
        assert_eq!(&with[app0_end..app0_end + 2], &[0xFF, 0xE1]);
    }

    #[test]
    fn saves_run_one_at_a_time() {
        let first = BigSaveTurn::wait();
        let (started, second_started) = std::sync::mpsc::channel();
        let second = std::thread::spawn(move || {
            let turn = BigSaveTurn::wait();
            started.send(()).expect("test alive");
            drop(turn);
        });
        assert!(
            second_started
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "a second save waits for the first"
        );
        drop(first);
        second_started
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the second save runs once the first ends");
        second.join().expect("second save");
    }

    #[test]
    fn typed_extension_decides_format() {
        assert_eq!(
            BigImageSaveFormat::for_path(Path::new("/tmp/a.PNG")),
            Some(BigImageSaveFormat::Png)
        );
        assert_eq!(
            BigImageSaveFormat::for_path(Path::new("/tmp/a.jpeg")),
            Some(BigImageSaveFormat::Jpeg)
        );
        assert_eq!(
            BigImageSaveFormat::for_path(Path::new("/tmp/a.webp")),
            Some(BigImageSaveFormat::Webp)
        );
        assert_eq!(
            BigImageSaveFormat::for_path(Path::new("/tmp/a.bmp")),
            None,
            "a format we do not write is refused, not silently written as another"
        );
        assert_eq!(BigImageSaveFormat::for_path(Path::new("/tmp/a")), None);
    }

    /// The two formats gdk-pixbuf carries itself are always there.
    #[test]
    fn png_and_jpeg_are_always_writable() {
        assert!(BigImageSaveFormat::Png.is_available());
        assert!(BigImageSaveFormat::Jpeg.is_available());
    }

    /// Whatever the system says it can write, it writes: the availability
    /// check and the encoder must not disagree.
    #[test]
    fn available_formats_round_trip() {
        let image = BigRgbaImage {
            rgba: vec![120; 8 * 8 * 4],
            width: 8,
            height: 8,
            cicp: None,
            opaque: false,
        };
        for format in BIG_IMAGE_SAVE_FORMATS {
            if !format.is_available() {
                continue;
            }
            let target = std::env::temp_dir().join(format!(
                "big-image-save-{}.{}",
                std::process::id(),
                format.extensions()[0]
            ));
            let _ = std::fs::remove_file(&target);
            save_rgba(&image, &target).unwrap_or_else(|error| {
                panic!("{} is offered but failed: {error}", format.extensions()[0])
            });
            #[allow(clippy::disallowed_methods, reason = "reads back what the test wrote")]
            let written = gtk::gdk_pixbuf::Pixbuf::from_file(&target)
                .unwrap_or_else(|_| panic!("{} did not decode again", format.extensions()[0]));
            assert_eq!((written.width(), written.height()), (8, 8));
            std::fs::remove_file(&target).unwrap();
        }
    }

    #[test]
    fn malformed_buffer_touches_no_file() {
        let target = std::env::temp_dir().join("big-image-save-refused.png");
        let _ = std::fs::remove_file(&target);
        let broken = BigRgbaImage {
            rgba: vec![0; 3],
            width: 2,
            height: 2,
            cicp: None,
            opaque: false,
        };
        assert!(save_rgba(&broken, &target).is_err());
        assert!(!target.exists(), "a refused save leaves no partial file");
    }

    /// Saving over a picture keeps what describes it, its tags here, and a
    /// link stays a link to the picture it names.
    #[test]
    fn save_over_keeps_tags_and_links() {
        use gtk::gio::prelude::FileExt;
        const TAGS: &str = "xattr::xdg.tags";
        let folder = std::env::temp_dir().join(format!(
            "big-image-save-over-{}",
            gtk::glib::uuid_string_random()
        ));
        std::fs::create_dir_all(&folder).expect("test directory");
        let target = folder.join("shot.png");
        let link = folder.join("link.png");
        std::fs::write(&target, b"old").expect("written");
        std::os::unix::fs::symlink(&target, &link).expect("link");
        let file = gtk::gio::File::for_path(&target);
        file.set_attribute_string(
            TAGS,
            "trip",
            gtk::gio::FileQueryInfoFlags::NONE,
            gtk::gio::Cancellable::NONE,
        )
        .expect("tagged");
        let image = BigRgbaImage {
            rgba: vec![120; 4 * 4 * 4],
            width: 4,
            height: 4,
            cicp: None,
            opaque: true,
        };

        save_rgba(&image, &link).expect("saved");

        let tags = file
            .query_info(
                TAGS,
                gtk::gio::FileQueryInfoFlags::NONE,
                gtk::gio::Cancellable::NONE,
            )
            .expect("queried")
            .attribute_string(TAGS);
        let is_link = std::fs::symlink_metadata(&link).is_ok_and(|meta| meta.is_symlink());
        let _ = std::fs::remove_dir_all(&folder);
        assert!(is_link);
        assert_eq!(tags.as_deref(), Some("trip"));
    }
}
