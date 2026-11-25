// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
use super::*;

const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16">
    <rect width="16" height="16" fill="#3584e4"/></svg>"##;

fn write_temp(name: &str, bytes: &[u8]) -> PathBuf {
    let path = std::env::temp_dir().join(format!("big-image-{}-{name}", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn vectors_render_at_the_bound() {
    let path = write_temp("crisp.svg", SVG.as_bytes());
    let image = decode_rgba_sandboxed(&BigImageSpec::bounded(&path, 256, 256)).expect("decodes");
    assert_eq!((image.width, image.height), (256, 256));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn raster_images_never_upscale() {
    assert_eq!(
        effective_decode_bound(Some((512, 512)), Some((100, 80)), false),
        Some((100, 80))
    );
    assert_eq!(effective_decode_bound(None, Some((10, 10)), false), None);
}

/// A 1170x710 screenshot asked for 256x256 came back square: Glycin scales
/// to exactly the size asked, so the size asked keeps the image's shape.
#[test]
fn decode_size_keeps_proportions() {
    assert_eq!(
        effective_decode_bound(Some((64, 64)), Some((1000, 500)), false),
        Some((64, 32))
    );
    assert_eq!(
        effective_decode_bound(Some((256, 256)), Some((1170, 710)), false),
        Some((256, 155))
    );
    // A vector is drawn up to the bound, still at its own shape.
    assert_eq!(
        effective_decode_bound(Some((64, 64)), Some((10, 5)), true),
        Some((64, 32))
    );
    // Unknown proportions leave only the bound.
    assert_eq!(
        effective_decode_bound(Some((64, 64)), None, false),
        Some((64, 64))
    );
}

#[test]
fn packed_rgba_round_trips() {
    big_testkit::harness::require_gtk_test(|| {
        let path = write_temp("rt.svg", SVG.as_bytes());
        let image = decode_rgba_sandboxed(&BigImageSpec::bounded(&path, 32, 32)).expect("decodes");
        assert_eq!(image.rgba.len(), 32 * 32 * 4);
        let texture = rgba_to_texture(&image).expect("texture");
        assert_eq!((texture.width(), texture.height()), (32, 32));
        let _ = std::fs::remove_file(&path);
    });
}

#[test]
fn sandbox_required_never_uses_pixbuf() {
    let require_sandbox = std::env::var_os("BIG_IMAGE_REQUIRE_SANDBOX_TESTS").is_some();
    let path = write_temp("glycin-required.svg", SVG.as_bytes());
    let result = decode_rgba_sandboxed(&BigImageSpec::bounded(&path, 16, 16));
    match result {
        Ok(image) => assert_eq!(
            (image.width, image.height, image.rgba.len()),
            (16, 16, 1024)
        ),
        Err(BigImageError::SandboxUnavailable) if !require_sandbox => {}
        Err(error) => panic!("valid SVG produced an unexpected strict error: {error}"),
    }

    std::fs::write(&path, b"not an image").unwrap();
    match decode_rgba_sandboxed(&BigImageSpec::bounded(&path, 16, 16)) {
        Err(BigImageError::SandboxedDecodeFailed) => {}
        Err(BigImageError::SandboxUnavailable) if !require_sandbox => {}
        result => panic!("corrupt SVG produced an unexpected strict result: {result:?}"),
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn sandbox_sniffs_svg_without_extension() {
    let require_sandbox = std::env::var_os("BIG_IMAGE_REQUIRE_SANDBOX_TESTS").is_some();
    let path = write_temp("extensionless-svg", SVG.as_bytes());
    match decode_rgba_sandboxed(&BigImageSpec::bounded(&path, 32, 32)) {
        Ok(image) => assert_eq!((image.width, image.height), (32, 32)),
        Err(BigImageError::SandboxUnavailable) if !require_sandbox => {}
        Err(error) => panic!("extensionless SVG produced an unexpected strict error: {error}"),
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn named_colour_space_reaches_texture() {
    big_testkit::harness::require_gtk_test(|| {
        let display_p3 = BigCicp {
            color_primaries: 12,
            transfer_characteristics: 13,
            matrix_coefficients: 0,
            full_range: true,
        };
        assert!(
            color_state_for(display_p3).is_some(),
            "Display-P3 is a colour space GDK knows"
        );

        let nonsense = BigCicp {
            color_primaries: 250,
            transfer_characteristics: 250,
            matrix_coefficients: 250,
            full_range: true,
        };
        assert!(
            color_state_for(nonsense).is_none(),
            "an unusable combination must not become a colour state"
        );

        // Whatever the metadata says, the pixels still reach the screen.
        let image = BigRgbaImage {
            rgba: vec![255, 0, 0, 255],
            width: 1,
            height: 1,
            cicp: Some(nonsense),
            opaque: false,
        };
        assert!(
            rgba_to_texture(&image).is_some(),
            "a refused colour space must not take the image off the screen"
        );
        let tagged = BigRgbaImage {
            cicp: Some(display_p3),
            ..image
        };
        let texture = rgba_to_texture(&tagged).expect("a tagged image still builds a texture");
        let expected = color_state_for(display_p3).expect("Display-P3 builds a colour state");
        assert_eq!(
            texture.color_state(),
            expected,
            "the texture must carry the colour space the decoder reported"
        );
        assert_ne!(
            texture.color_state(),
            gdk::ColorState::srgb(),
            "a Display-P3 image handed over as sRGB is the bug this prevents"
        );
    });
}

#[test]
fn opaque_rgb_packs_to_rgbx() {
    // Two RGB pixels, tightly packed (stride == width * 3). The alpha-less
    // source becomes four-byte RGBX with a full alpha byte and the opaque flag,
    // so its texture is tagged R8g8b8x8 rather than premultiplied on upload.
    let source = [10, 20, 30, 40, 50, 60];
    let image = pack_rgb_rows_to_rgbx(&source, 2, 1, 6).expect("well-formed rgb rows");
    assert_eq!(image.rgba, [10, 20, 30, 255, 40, 50, 60, 255]);
    assert!(image.opaque);
}

#[test]
fn simd_and_scalar_rgb_packing_agree() {
    // Widths from none to ten groups of four pixels cover every tail the
    // vector loop leaves to the plain one; the padded stride checks that only
    // each row's pixels are read.
    let pixel = |at: usize| [(at * 7) as u8, (at * 13 + 1) as u8, (at * 29 + 2) as u8];
    for width in 0..=40 {
        let source: Vec<u8> = (0..width).flat_map(pixel).collect();
        let expected: Vec<u8> = (0..width)
            .flat_map(|at| {
                let [r, g, b] = pixel(at);
                [r, g, b, 255]
            })
            .collect();
        let mut plain = vec![0; width * 4];
        crate::pack_rgb_row(&source, &mut plain);
        assert_eq!(plain, expected, "plain loop, width {width}");
        let mut chosen = vec![0; width * 4];
        crate::rgb_row_packer()(&source, &mut chosen);
        assert_eq!(
            chosen, expected,
            "packer chosen for this processor, width {width}"
        );
    }
    let (width, height, stride) = (37, 5, 37 * 3 + 5);
    let source: Vec<u8> = (0..stride * height).map(|at| (at * 31) as u8).collect();
    let image = pack_rgb_rows_to_rgbx(&source, 37, 5, stride).expect("padded rgb rows");
    for y in 0..height {
        for x in 0..width {
            let from = y * stride + x * 3;
            let to = (y * width + x) * 4;
            assert_eq!(
                image.rgba[to..to + 4],
                [source[from], source[from + 1], source[from + 2], 255]
            );
        }
    }
}

#[test]
fn purged_icon_cache_accepts_reput() {
    let texture = rgba_to_texture(&BigRgbaImage {
        rgba: vec![1, 2, 3, 255],
        width: 1,
        height: 1,
        cicp: None,
        opaque: false,
    })
    .expect("1x1 texture");
    crate::icon::memory_put("icon-purge-test".to_owned(), texture.clone());
    assert!(crate::icon::memory_hit("icon-purge-test").is_some());
    crate::icon::purge_memory_cache();
    // Something still draws it, so it is still the one to hand out.
    assert!(crate::icon::memory_hit("icon-purge-test").is_some());
    drop(texture);
    assert!(
        crate::icon::memory_hit("icon-purge-test").is_none(),
        "purge must drop a texture nothing else holds"
    );
}

#[test]
fn undrawn_icons_stay_within_recent_budget() {
    let tile = || {
        rgba_to_texture(&BigRgbaImage {
            rgba: vec![7; 96 * 96 * 4],
            width: 96,
            height: 96,
            cicp: None,
            opaque: false,
        })
        .expect("96x96 texture")
    };
    // 300 full-screen-menu-sized icons, none held by a widget: about 11 MB, where
    // the recent budget is 4 MB.
    for index in 0..300 {
        crate::icon::memory_put(format!("icon-budget-{index}"), tile());
    }
    let kept = (0..300)
        .filter(|index| crate::icon::memory_hit(&format!("icon-budget-{index}")).is_some())
        .count();
    assert!(kept < 120, "{kept} of 300 unheld icons were kept");
    assert!(
        crate::icon::memory_hit("icon-budget-299").is_some(),
        "the most recent are the ones kept"
    );
    crate::icon::purge_memory_cache();
}

/// A PNG past the display ceiling opens reduced: its loader may deliver the
/// whole frame however small the size asked.
#[test]
fn huge_png_decodes_reduced() {
    let path = std::env::temp_dir().join(format!("big-image-{}-huge.png", std::process::id()));
    let (width, height) = (8200, 8000);
    let file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::One);
    let mut writer = encoder.write_header().unwrap();
    writer
        .write_image_data(&vec![0x55; (width as usize).div_ceil(8) * height as usize])
        .unwrap();
    writer.finish().unwrap();
    let decoded = decode_rgba_sandboxed(&BigImageSpec::bounded(&path, 820, 800));
    let _ = std::fs::remove_file(&path);
    let image = decoded.expect("decodes");
    assert_eq!((image.width, image.height), (820, 800));
}

/// Whether a 2×2 PNG written with `describe` applied to its header decodes
/// to a frame whose copy leaves a colour description out.
fn loses_colour_description(name: &str, describe: impl FnOnce(&mut png::Info)) -> bool {
    let path = std::env::temp_dir().join(format!("big-image-{}-{name}.png", std::process::id()));
    let mut info = png::Info::with_size(2, 2);
    info.color_type = png::ColorType::Rgb;
    describe(&mut info);
    let file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    let mut writer = png::Encoder::with_info(file, info)
        .unwrap()
        .write_header()
        .unwrap();
    writer.write_image_data(&[0x80; 12]).unwrap();
    writer.finish().unwrap();
    let decoded = decode_rgba_sandboxed_cancellable(
        &BigImageSpec::bounded(&path, 2, 2),
        std::sync::Arc::default(),
        big_os_kit::session_resources::shared(),
        big_os_kit::session_resources::MemoryClass::Interactive,
        0,
    );
    let _ = std::fs::remove_file(&path);
    decoded.expect("decodes").1.colour_profile
}

/// A frame converted from an ICC profile says so: a copy written from it
/// leaves the profile out.
#[test]
fn icc_source_reports_its_profile() {
    assert!(!loses_colour_description("plain", |_| {}));
    assert!(loses_colour_description("icc", |info| {
        info.icc_profile = Some(std::borrow::Cow::Borrowed(
            b"not parsed by the test, only carried",
        ));
    }));
}

/// An sRGB profile changes no colour, so its copy asks nothing.
#[test]
fn srgb_profile_loses_nothing() {
    // A version 2 profile holding only its description tag.
    let description = b"sRGB IEC61966-2.1\0";
    let mut icc = vec![0; 128];
    icc.extend_from_slice(&1_u32.to_be_bytes());
    icc.extend_from_slice(b"desc");
    icc.extend_from_slice(&144_u32.to_be_bytes());
    icc.extend_from_slice(&(12 + description.len() as u32).to_be_bytes());
    icc.extend_from_slice(b"desc\0\0\0\0");
    icc.extend_from_slice(&(description.len() as u32).to_be_bytes());
    icc.extend_from_slice(description);
    assert!(!loses_colour_description("srgb-icc", |info| {
        info.icc_profile = Some(std::borrow::Cow::Owned(icc));
    }));
}

/// Pixels CICP places outside sRGB are written converted, so the copy says
/// it leaves that description out. The fixtures are 2×2 AVIFs tagged Display
/// P3 and sRGB.
#[test]
fn cicp_source_reports_its_profile() {
    let loses = |name, bytes: &[u8]| {
        let path = write_temp(name, bytes);
        let decoded = decode_rgba_sandboxed_cancellable(
            &BigImageSpec::bounded(&path, 2, 2),
            std::sync::Arc::default(),
            big_os_kit::session_resources::shared(),
            big_os_kit::session_resources::MemoryClass::Interactive,
            0,
        );
        let _ = std::fs::remove_file(&path);
        decoded.expect("decodes").1.colour_profile
    };
    assert!(loses(
        "p3.avif",
        include_bytes!("../tests/fixtures/p3.avif")
    ));
    assert!(!loses(
        "srgb.avif",
        include_bytes!("../tests/fixtures/srgb.avif")
    ));
}

/// A PQ picture keeps its 16 bits and its colour state, at its own size and
/// shrunk. The fixture is a 288×162 lossless 10-bit AVIF, BT.2020 and PQ:
/// nine 32-pixel bars of the codes `big-compositor/tests/hdr/oracle.py`
/// sends, encoded by avifenc from a 16-bit PNG with those codes.
#[test]
fn pq_texture_keeps_16_bits() {
    const BARS: [[u16; 3]; 9] = [
        [520, 520, 520],
        [594, 594, 594],
        [668, 668, 668],
        [769, 769, 769],
        [668, 0, 0],
        [0, 668, 0],
        [0, 0, 668],
        [769, 0, 0],
        [545, 335, 225],
    ];
    let path = write_temp("pq.avif", include_bytes!("../tests/fixtures/pq.avif"));
    for (width, height) in [(288, 162), (144, 81)] {
        let texture = glib::MainContext::new()
            .block_on(load_sandboxed_texture_cancellable(
                BigImageSpec::bounded(&path, width, height),
                BigImageFit::Contain,
                gio::Cancellable::new(),
            ))
            .expect("decodes");
        assert_eq!((texture.width(), texture.height()), (width, height));
        assert_eq!(texture.format(), gdk::MemoryFormat::R16g16b16);
        let cicp = texture
            .color_state()
            .create_cicp_params()
            .expect("CICP colour state");
        assert_eq!((cicp.color_primaries(), cicp.transfer_function()), (9, 16));
        let mut downloader = gdk::TextureDownloader::new(&texture);
        downloader.set_format(gdk::MemoryFormat::R16g16b16);
        downloader.set_color_state(&texture.color_state());
        let (bytes, stride) = downloader.download_bytes();
        let bar = usize::try_from(width).unwrap() / BARS.len();
        for (at, want) in BARS.iter().enumerate() {
            let start = (usize::try_from(height).unwrap() / 2) * stride + (at * bar + bar / 2) * 6;
            let code = |channel: usize| {
                u16::from_ne_bytes([bytes[start + channel * 2], bytes[start + channel * 2 + 1]])
                    >> 6
            };
            assert_eq!(
                [code(0), code(1), code(2)],
                *want,
                "bar {at} at {width}×{height}"
            );
        }
    }
    let _ = std::fs::remove_file(&path);
}

/// A copy is written in sRGB: pixels in a wider space are converted, not
/// passed off as sRGB.
#[test]
fn cicp_copy_is_written_srgb() {
    let image = BigRgbaImage {
        rgba: vec![200, 100, 50, 255],
        width: 1,
        height: 1,
        cicp: Some(BigCicp {
            color_primaries: 12,
            transfer_characteristics: 13,
            matrix_coefficients: 0,
            full_range: true,
        }),
        opaque: true,
    };
    let encoded = encode_rgba(&image, BigImageSaveFormat::Png).expect("encodes");
    let mut reader = png::Decoder::new(std::io::Cursor::new(encoded))
        .read_info()
        .unwrap();
    let mut pixel = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut pixel).unwrap();
    assert_ne!(&pixel[..3], &[200, 100, 50]);
    assert!(
        pixel[0] > 200,
        "Display P3 red is redder than sRGB's: {pixel:?}"
    );
}

fn governor(bytes: usize) -> std::sync::Arc<big_os_kit::session_resources::SessionResources> {
    big_os_kit::session_resources::SessionResources::new(
        big_os_kit::session_resources::ResourceLimits {
            bytes: [bytes; 2],
            optional_bytes: [bytes / 2; 2],
            display_frame_bytes: bytes,
            jobs: 1,
            background_jobs: 1,
        },
    )
}

/// A 1000x1000 PNG: the loader delivers all 4 MB however small the bound.
fn write_square_png(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("big-image-{}-{name}", std::process::id()));
    let mut writer = png::Encoder::new(
        std::io::BufWriter::new(std::fs::File::create(&path).unwrap()),
        1000,
        1000,
    );
    writer.set_color(png::ColorType::Rgb);
    writer.set_depth(png::BitDepth::Eight);
    let mut writer = writer.write_header().unwrap();
    writer.write_image_data(&vec![0x40; 3_000_000]).unwrap();
    writer.finish().unwrap();
    path
}

fn decode_small(
    path: &std::path::Path,
    resources: &std::sync::Arc<big_os_kit::session_resources::SessionResources>,
) -> Result<(BigRgbaImage, BigSourceSize), BigImageError> {
    decode_small_holding(path, resources, 0)
}

fn decode_small_holding(
    path: &std::path::Path,
    resources: &std::sync::Arc<big_os_kit::session_resources::SessionResources>,
    held: usize,
) -> Result<(BigRgbaImage, BigSourceSize), BigImageError> {
    decode_rgba_sandboxed_cancellable(
        &BigImageSpec::bounded(path, 64, 64),
        std::sync::Arc::default(),
        resources,
        big_os_kit::session_resources::MemoryClass::Interactive,
        held,
    )
}

/// A frame larger than the whole governor opens when nothing else is held.
#[test]
fn oversize_full_frame_opens_alone() {
    let path = write_square_png("whole-too-large.png");
    let result = decode_small(&path, &governor(2 * 1024 * 1024));
    let _ = std::fs::remove_file(&path);
    assert!(result.is_ok(), "{:?}", result.err());
}

/// The display frame the caller holds does not count against its own full frame.
#[test]
fn own_display_frame_allows_oversize_open() {
    use big_os_kit::session_resources::{MemoryClass, MemoryDomain};
    let path = write_square_png("whole-own-lease.png");
    let resources = governor(2 * 1024 * 1024);
    let held = 16 * 1024;
    let _display = resources
        .reserve(MemoryDomain::Cpu, MemoryClass::Interactive, held)
        .unwrap();
    let result = decode_small_holding(&path, &resources, held);
    let _ = std::fs::remove_file(&path);
    assert!(result.is_ok(), "{:?}", result.err());
}

/// The frame a loader cannot reduce is alive while it is scaled, so a
/// governor that is busy elsewhere refuses the open.
#[test]
fn full_frame_counts_against_admission() {
    use big_os_kit::session_resources::{MemoryClass, MemoryDomain};
    let path = write_square_png("whole-refused.png");
    let resources = governor(5 * 1024 * 1024);
    let _busy = resources
        .reserve(MemoryDomain::Cpu, MemoryClass::Interactive, 2 * 1024 * 1024)
        .unwrap();
    let result = decode_small(&path, &resources);
    let _ = std::fs::remove_file(&path);
    assert!(matches!(result, Err(BigImageError::ResourceLimit)));
}

/// A vector is drawn at the size asked for, so no full frame exists to charge.
#[test]
fn declared_svg_size_is_not_charged() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10000" height="10000">
        <rect width="10000" height="10000" fill="#3584e4"/></svg>"##;
    let path = write_temp("huge-declared.svg", svg.as_bytes());
    let result = decode_small(&path, &governor(2 * 1024 * 1024));
    let _ = std::fs::remove_file(&path);
    assert!(result.is_ok(), "{:?}", result.err());
}

/// The charge for the full frame ends with the scale-down.
#[test]
fn full_frame_charge_is_released() {
    let path = write_square_png("whole-released.png");
    let resources = governor(16 * 1024 * 1024);
    let decoded = decode_small(&path, &resources);
    let _ = std::fs::remove_file(&path);
    assert_eq!(decoded.expect("decodes").0.width, 64);
    assert_eq!(resources.snapshot().bytes, [[0; 4]; 2]);
}
