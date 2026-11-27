use glycin_common::{MemoryFormat, MemoryFormatInfo};

use crate::{ColorState, Error};

pub fn apply_transformation(
    icc_profile: &[u8],
    memory_format: MemoryFormat,
    mmap: &mut [u8],
) -> Result<ColorState, Error> {
    transform(icc_profile, memory_format, mmap).map_err(Into::into)
}

/// Pixels each thread transforms, at least: about 1.3 ms of lcms2 work, well
/// above what spawning the thread costs. A thumbnail stays on the calling
/// thread instead of paying one transform build per core.
const PIXELS_PER_THREAD: usize = 131_072;

/// The transform from `icc_profile` to sRGB (gray for one- and two-channel
/// formats). Built without lcms2's last-pixel cache, which makes it `Sync`, so
/// one build serves every thread.
fn transformation(
    icc_profile: &[u8],
    memory_format: MemoryFormat,
) -> std::result::Result<
    lcms2::Transform<u8, u8, lcms2::GlobalContext, lcms2::DisallowCache>,
    lcms2::Error,
> {
    tracing::debug!("Converting to sRGB via ICC profile");

    let icc_pixel_format = lcms_pixel_format(memory_format);
    let src_profile = lcms2::Profile::new_icc(icc_profile)?;

    let target_profile;

    if memory_format.n_channels() > 2 {
        target_profile = lcms2::Profile::new_srgb();
    } else {
        target_profile =
            lcms2::Profile::new_gray(lcms2_sys::ffi::CIExyY::d50(), &lcms2::ToneCurve::new(2.2))?;
    };

    lcms2::Transform::new_flags_context(
        lcms2::GlobalContext::new(),
        &src_profile,
        icc_pixel_format,
        &target_profile,
        icc_pixel_format,
        lcms2::Intent::Perceptual,
        lcms2::Flags::NO_CACHE,
    )
}

fn transform(
    icc_profile: &[u8],
    memory_format: MemoryFormat,
    buf: &mut [u8],
) -> std::result::Result<ColorState, lcms2::Error> {
    let transform = transformation(icc_profile, memory_format)?;
    let pixel = memory_format.n_bytes().usize();
    let pixels = buf.len() / pixel;
    // A loader may hand a buffer longer than its frame; lcms2 panics on a
    // partial pixel.
    let buf = &mut buf[..pixels * pixel];
    let threads = (pixels / PIXELS_PER_THREAD).clamp(
        1,
        std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
    );
    tracing::trace!("Applying ICC profiles while using {threads} threads");

    if threads == 1 {
        transform.transform_in_place(buf);
    } else {
        let transform = &transform;
        std::thread::scope(|s| {
            for chunk in buf.chunks_mut(pixels.div_ceil(threads) * pixel) {
                s.spawn(move || transform.transform_in_place(chunk));
            }
        });
    }

    Ok(ColorState::Srgb)
}

const fn lcms_pixel_format(format: MemoryFormat) -> lcms2::PixelFormat {
    match format {
        MemoryFormat::B8g8r8a8Premultiplied => premul(lcms2::PixelFormat::BGRA_8),
        MemoryFormat::A8r8g8b8Premultiplied => premul(lcms2::PixelFormat::ARGB_8),
        MemoryFormat::R8g8b8a8Premultiplied => premul(lcms2::PixelFormat::RGBA_8),
        MemoryFormat::B8g8r8a8 => lcms2::PixelFormat::BGRA_8,
        MemoryFormat::A8r8g8b8 => lcms2::PixelFormat::ARGB_8,
        MemoryFormat::R8g8b8a8 => lcms2::PixelFormat::RGBA_8,
        MemoryFormat::A8b8g8r8 => lcms2::PixelFormat::ABGR_8,
        MemoryFormat::R8g8b8 => lcms2::PixelFormat::RGB_8,
        MemoryFormat::B8g8r8 => lcms2::PixelFormat::BGR_8,
        MemoryFormat::R16g16b16 => lcms2::PixelFormat::RGB_16,
        MemoryFormat::R16g16b16a16Premultiplied => premul(lcms2::PixelFormat::RGBA_16),
        MemoryFormat::R16g16b16a16 => lcms2::PixelFormat::RGBA_16,
        MemoryFormat::R16g16b16Float => lcms2::PixelFormat::RGB_HALF_FLT,
        MemoryFormat::R16g16b16a16Float => lcms2::PixelFormat::RGBA_HALF_FLT,
        MemoryFormat::R32g32b32Float => lcms2::PixelFormat::RGB_FLT,
        MemoryFormat::R32g32b32a32FloatPremultiplied => premul(lcms2::PixelFormat::RGBA_FLT),
        MemoryFormat::R32g32b32a32Float => lcms2::PixelFormat::RGBA_FLT,
        MemoryFormat::G8a8Premultiplied => premul(lcms2::PixelFormat::GRAYA_8),
        MemoryFormat::G8a8 => lcms2::PixelFormat::GRAYA_8,
        MemoryFormat::G8 => lcms2::PixelFormat::GRAY_8,
        MemoryFormat::G16a16Premultiplied => premul(lcms2::PixelFormat::GRAYA_16),
        MemoryFormat::G16a16 => lcms2::PixelFormat::GRAYA_16,
        MemoryFormat::G16 => lcms2::PixelFormat::GRAY_16,
    }
}

const fn premul(format: lcms2::PixelFormat) -> lcms2::PixelFormat {
    let mut bytes = format.0;
    bytes |= 0b1 << 23;
    lcms2::PixelFormat(bytes)
}

#[test]
fn premul_test() {
    assert!(!lcms2::PixelFormat::RGBA_8.premultiplied());
    assert!(premul(lcms2::PixelFormat::RGBA_8).premultiplied());
}

/// A frame split across threads comes out as transformed whole: every chunk
/// starts on a pixel and the last one takes the remainder.
#[test]
fn chunked_matches_whole() {
    use lcms2_sys::ffi::{CIExyY, CIExyYTRIPLE};

    let primary = |x, y| CIExyY { x, y, Y: 1.0 };
    let gamma = lcms2::ToneCurve::new(2.2);
    let wide = lcms2::Profile::new_rgb(
        &primary(0.3127, 0.3290),
        &CIExyYTRIPLE {
            Red: primary(0.64, 0.33),
            Green: primary(0.21, 0.71),
            Blue: primary(0.15, 0.06),
        },
        &[&gamma, &gamma, &gamma],
    )
    .and_then(|profile| profile.icc())
    .unwrap();
    let pixels = 2 * PIXELS_PER_THREAD + 7;
    let source: Vec<u8> = (0..pixels * 4).map(|i| (i * 7 % 251) as u8).collect();

    let mut chunked = source.clone();
    transform(&wide, MemoryFormat::R8g8b8a8, &mut chunked).unwrap();
    let mut whole = source.clone();
    transformation(&wide, MemoryFormat::R8g8b8a8)
        .unwrap()
        .transform_in_place(&mut whole);

    assert_ne!(whole, source, "the profile changes the pixels");
    assert!(chunked == whole, "chunked and whole transforms differ");
}

/// Bytes past the last whole pixel, which a loader may leave, are not handed
/// to lcms2.
#[test]
fn partial_pixel_left_alone() {
    let srgb = lcms2::Profile::new_srgb().icc().unwrap();
    let mut buf = vec![10, 20, 30, 40];
    transform(&srgb, MemoryFormat::R8g8b8, &mut buf).unwrap();
    assert_eq!(buf[3], 40);
}
