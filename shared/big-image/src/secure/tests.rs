// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
use std::path::PathBuf;

use super::*;

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("big-image-secure-{}-{name}", std::process::id()))
}

#[test]
fn strict_request_stages_regular_file() {
    use std::os::unix::fs::PermissionsExt;

    let path = temp_path("regular");
    std::fs::write(&path, b"staged bytes").unwrap();
    let staged = stage_request(&BigImageSpec::bounded(&path, 800, 600)).unwrap();
    assert_eq!(
        staged
            .source
            .private_directory
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    // The decode path is a symlink onto the pinned descriptor, not a copy:
    // reading it must reach the bytes without any having been written.
    assert!(
        std::fs::symlink_metadata(&staged.source.decode_path)
            .unwrap()
            .is_symlink()
    );
    assert!(
        std::fs::read_link(&staged.source.decode_path)
            .unwrap()
            .starts_with("/proc/self/fd/")
    );
    assert_eq!(
        std::fs::read(&staged.source.decode_path).unwrap(),
        b"staged bytes"
    );
    assert_eq!(staged.bound, (800, 600));
    let decode_path = staged.source.decode_path.clone();
    let private_directory = staged.source.private_directory.clone();
    drop(staged);
    assert!(!decode_path.exists());
    assert!(!private_directory.exists());
    let _ = std::fs::remove_file(path);
}

#[test]
fn staged_identity_survives_path_replacement() {
    let path = temp_path("replace");
    let replacement = temp_path("replacement");
    std::fs::write(&path, b"original").unwrap();
    let staged = stage_request(&BigImageSpec::bounded(&path, 1, 1)).unwrap();
    std::fs::write(&replacement, b"replacement").unwrap();
    big_os_kit::file_io::rename(&replacement, &path).unwrap();
    // What the decoder opens is the descriptor this staging validated, so
    // swapping the path underneath cannot change the bytes it sees.
    assert_eq!(
        std::fs::read(&staged.source.decode_path).unwrap(),
        b"original"
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn staging_keeps_only_safe_suffix() {
    assert_eq!(safe_decode_suffix(Path::new("/tmp/image.SVG")), ".svg");
    assert_eq!(
        safe_decode_suffix(Path::new("/tmp/image.svg.gz")),
        ".svg.gz"
    );
    assert_eq!(safe_decode_suffix(Path::new("/tmp/image.bad-name")), "");
    assert_eq!(safe_decode_suffix(Path::new("/tmp/image")), "");
}

#[test]
fn strict_request_rejects_unsafe_sources() {
    let directory = temp_path("directory");
    std::fs::create_dir_all(&directory).unwrap();
    assert!(matches!(
        stage_request(&BigImageSpec::bounded(
            "https://example.test/image.png",
            1,
            1
        )),
        Err(BigImageError::InvalidSource { .. })
    ));
    assert!(matches!(
        stage_request(&BigImageSpec::bounded(&directory, 1, 1)),
        Err(BigImageError::InvalidSource { .. })
    ));
    assert!(matches!(
        stage_request(&BigImageSpec {
            bound: None,
            ..BigImageSpec::bounded(&directory, 1, 1)
        }),
        Err(BigImageError::InvalidDisplayBound)
    ));
    assert!(matches!(
        validate_path_shape(Path::new("/tmp/parent/../image.png")),
        Err(BigImageError::InvalidSource { .. })
    ));
    assert!(matches!(
        validate_path_shape(Path::new("relative.png")),
        Err(BigImageError::InvalidSource { .. })
    ));
    let _ = std::fs::remove_dir(directory);
}

#[test]
fn strict_request_rejects_oversized_sparse_source() {
    let path = temp_path("oversized");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(MAX_SOURCE_BYTES + 1).unwrap();
    assert!(matches!(
        stage_request(&BigImageSpec::bounded(&path, 1, 1)),
        Err(BigImageError::SourceTooLarge { .. })
    ));
    let _ = std::fs::remove_file(path);
}

#[test]
fn dimension_limits_reject_bad_sizes() {
    assert!(matches!(
        validate_bound(Some((0, 10)), MAX_DISPLAY_PIXELS),
        Err(BigImageError::InvalidDisplayBound)
    ));
    assert!(matches!(
        validate_bound(Some((i32::MAX, i32::MAX)), MAX_DISPLAY_PIXELS),
        Err(BigImageError::PixelLimitExceeded { .. })
    ));
    assert!(matches!(
        validate_source_dimensions(u32::MAX, u32::MAX),
        Err(BigImageError::PixelLimitExceeded { .. })
    ));
}

#[test]
fn fit_sizes_never_upscale_rasters() {
    assert_eq!(
        effective_texture_size((1920, 1080), (3840, 2160), false, BigImageFit::Contain),
        Some((1920, 1080))
    );
    assert_eq!(
        effective_texture_size((1920, 1080), (4000, 3000), false, BigImageFit::Cover),
        Some((1920, 1440))
    );
    assert_eq!(
        effective_texture_size((1920, 1080), (3000, 4000), false, BigImageFit::Cover),
        Some((1920, 2560))
    );
    assert_eq!(
        effective_texture_size((1920, 1080), (4000, 3000), false, BigImageFit::Fill),
        Some((1920, 1080))
    );
    // A raster smaller than the output carries no extra detail to decode.
    assert_eq!(
        effective_texture_size((1920, 1080), (800, 600), false, BigImageFit::Cover),
        Some((800, 600))
    );
    // A scalable source can be rendered directly at the requested cover size.
    assert_eq!(
        effective_texture_size((1920, 1080), (100, 200), true, BigImageFit::Cover),
        Some((1920, 3840))
    );
}

#[cfg(unix)]
#[test]
fn strict_request_rejects_special_files() {
    use std::os::unix::fs::symlink;

    let target = temp_path("target");
    let link = temp_path("symlink");
    // In `/tmp`: a socket path must fit `sun_path` (108 bytes), and `TMPDIR` can be long.
    let socket = std::path::Path::new("/tmp")
        .join(format!("big-image-secure-{}-socket", std::process::id()));
    std::fs::write(&target, b"image").unwrap();
    symlink(&target, &link).unwrap();
    assert!(matches!(
        stage_request(&BigImageSpec::bounded(&link, 1, 1)),
        Err(BigImageError::InvalidSource { .. })
    ));
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    assert!(matches!(
        stage_request(&BigImageSpec::bounded(&socket, 1, 1)),
        Err(BigImageError::InvalidSource { .. })
    ));
    drop(listener);
    let _ = std::fs::remove_file(target);
    let _ = std::fs::remove_file(link);
    let _ = std::fs::remove_file(socket);
}

/// Set in the child that [`decoding_confined_without_user_namespaces`] starts.
const WITHOUT_USER_NAMESPACES: &str = "BIG_IMAGE_TEST_WITHOUT_USER_NAMESPACES";

const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16" fill="#3584e4"/></svg>"##;
/// A still 1×1 GIF.
const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\xff\xff\xff\x00\x00\x00!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";
const JPEG: &[u8] = include_bytes!("../../tests/fixtures/blue.jpg");

/// Where bubblewrap cannot make a namespace, glycin's loader confines itself
/// with seccomp and Landlock, and every entry point decodes through it.
#[test]
fn decoding_confined_without_user_namespaces() {
    if std::env::var_os(WITHOUT_USER_NAMESPACES).is_some() {
        every_entry_point_decodes();
    } else if Path::new(big_os_kit::confinement::BWRAP).exists() {
        run_without_user_namespaces();
    }
}

/// This test again, in a namespace that may not make another, where
/// bubblewrap fails the way it does with user namespaces off.
fn run_without_user_namespaces() {
    use big_os_kit::subprocess::BigSubprocessSpec;

    // Named as our JPEG loader's directory, so a region decoder is asked for.
    let mut data_dirs = glib::user_runtime_dir()
        .join("big-image/confined-test/jpg")
        .into_os_string();
    data_dirs.push(":/usr/share");
    let out = BigSubprocessSpec::builder()
        .program(big_os_kit::confinement::BWRAP)
        .args(["--dev-bind", "/", "/", "--unshare-user", "--disable-userns"])
        .arg("--")
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "secure::tests::decoding_confined_without_user_namespaces",
            "--quiet",
        ])
        .env(WITHOUT_USER_NAMESPACES, "1")
        .env("XDG_DATA_DIRS", data_dirs)
        .timeout(std::time::Duration::from_secs(60))
        .allow_list([big_os_kit::confinement::BWRAP])
        .build()
        .run()
        .unwrap();
    assert!(
        out.status.success() && out.stdout_lossy().contains("1 passed"),
        "{}\n{}",
        out.stdout_lossy(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn every_entry_point_decodes() {
    use gtk::prelude::TextureExt;

    let svg = temp_path("confined.svg");
    std::fs::write(&svg, SVG).unwrap();
    let gif = temp_path("confined.gif");
    std::fs::write(&gif, GIF).unwrap();
    let jpeg = temp_path("confined.jpg");
    std::fs::write(&jpeg, JPEG).unwrap();
    let spec = BigImageSpec::bounded(&svg, 16, 16);

    glib::MainContext::new().block_on(async {
        assert_eq!(
            glycin::SandboxMechanism::detect().await,
            glycin::SandboxMechanism::NotSandboxed
        );
    });
    assert_eq!(crate::decode_rgba_sandboxed(&spec).unwrap().width, 16);
    assert_eq!(crate::decode_rgba_for_save(&spec, 256).unwrap().width, 16);
    assert_eq!(
        crate::read_metadata_sandbox_required(&svg)
            .unwrap()
            .pixel_size,
        Some((16, 16))
    );
    glib::MainContext::new().block_on(async {
        let decoded = crate::decode_rgba_sandboxed_async(&spec).await;
        assert_eq!(decoded.unwrap().width, 16);
        let texture = crate::load_sandboxed_texture(spec.clone()).await.unwrap();
        assert_eq!(texture.width(), 16);
        let texture = crate::load_sandboxed_texture_cancellable(
            spec.clone(),
            BigImageFit::Contain,
            gio::Cancellable::new(),
        )
        .await
        .unwrap();
        assert_eq!(texture.width(), 16);
        let decoded = decode_jpeg_bytes_async(JPEG.to_vec(), 16, None).await;
        assert_eq!(decoded.unwrap().width, 16);
        let animation = crate::open_animation(BigImageSpec::bounded(&gif, 16, 16)).await;
        assert!(animation.unwrap().is_none(), "a still GIF");
        assert!(crate::open_region_decoder(&jpeg).await.is_ok());
        let icon = crate::icon::decode_svg(glib::Bytes::from_static(SVG), 16).await;
        assert_eq!(icon.unwrap().height(), 16);
    });
    for path in [svg, gif, jpeg] {
        let _ = std::fs::remove_file(path);
    }
}
