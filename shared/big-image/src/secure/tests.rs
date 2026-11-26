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
        stage_request(&BigImageSpec::natural(&directory)),
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

/// Set in the child that [`decoding_refused_without_sandbox`] starts.
const WITHOUT_SANDBOX: &str = "BIG_IMAGE_TEST_WITHOUT_SANDBOX";

const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16" fill="#3584e4"/></svg>"##;

fn assert_unavailable(error: Option<BigImageError>) {
    assert!(
        matches!(error, Some(BigImageError::SandboxUnavailable)),
        "{error:?}"
    );
}

/// Where bubblewrap cannot run, every entry point refuses before a loader
/// starts: the child finds no loader, so a load that went ahead would fail
/// as a decode instead.
#[test]
fn decoding_refused_without_sandbox() {
    if std::env::var_os(WITHOUT_SANDBOX).is_some() {
        every_entry_point_refuses();
    } else if Path::new(big_os_kit::confinement::BWRAP).exists() {
        run_without_user_namespaces();
    }
}

/// This test again, in a namespace that may not make another, where
/// bubblewrap fails the way it does with user namespaces off. Without
/// bubblewrap glycin still names it the sandbox, and a load fails as a decode.
fn run_without_user_namespaces() {
    use big_os_kit::subprocess::BigSubprocessSpec;

    let nowhere = glib::user_runtime_dir().join("big-image/unconfined-test");
    // Named as our JPEG loader's directory, so a region decoder is asked
    // for; nothing is read from it before the refusal.
    let mut data_dirs = nowhere.join("jpg").into_os_string();
    data_dirs.push(":/usr/share");
    let out = BigSubprocessSpec::builder()
        .program(big_os_kit::confinement::BWRAP)
        .args(["--dev-bind", "/", "/", "--unshare-user", "--disable-userns"])
        .arg("--")
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "secure::tests::decoding_refused_without_sandbox",
            "--quiet",
        ])
        .env(WITHOUT_SANDBOX, "1")
        .env("GLYCIN_DATA_DIR", nowhere)
        .env("XDG_DATA_DIRS", data_dirs)
        .timeout(std::time::Duration::from_secs(60))
        .allow_list([big_os_kit::confinement::BWRAP])
        .build()
        .run()
        .unwrap();
    assert!(
        out.status.success() && out.stdout_lossy().contains("1 passed"),
        "{}",
        out.stdout_lossy()
    );
}

fn every_entry_point_refuses() {
    let svg = temp_path("unconfined.svg");
    std::fs::write(&svg, SVG).unwrap();
    let gif = temp_path("unconfined.gif");
    std::fs::write(&gif, b"GIF89a").unwrap();
    let spec = BigImageSpec::bounded(&svg, 16, 16);

    assert_unavailable(crate::decode_rgba_sandboxed(&spec).err());
    assert_unavailable(crate::decode_rgba_for_save(&spec, 256).err());
    assert_unavailable(crate::read_metadata_sandbox_required(&svg).err());
    glib::MainContext::new().block_on(async {
        assert_unavailable(crate::decode_rgba_sandboxed_async(&spec).await.err());
        assert_unavailable(crate::load_sandboxed_texture(spec.clone()).await.err());
        assert_unavailable(
            crate::load_sandboxed_texture_cancellable(
                spec.clone(),
                BigImageFit::Contain,
                gio::Cancellable::new(),
            )
            .await
            .err(),
        );
        assert_unavailable(
            decode_jpeg_bytes_async(vec![0xFF, 0xD8, 0xFF], 16, None)
                .await
                .err(),
        );
        assert_unavailable(
            crate::open_animation(BigImageSpec::bounded(&gif, 16, 16))
                .await
                .err(),
        );
        assert_unavailable(
            crate::open_region_decoder(temp_path("unconfined.jpg"))
                .await
                .err(),
        );
    });
    let _ = std::fs::remove_file(svg);
    let _ = std::fs::remove_file(gif);
}

/// A loader that came up unconfined is refused before it hands over a pixel.
#[test]
fn unconfined_loader_is_refused() {
    use glycin::SandboxMechanism::NotSandboxed;

    let svg = temp_path("loader-unconfined.svg");
    std::fs::write(&svg, SVG).unwrap();
    let spec = BigImageSpec::bounded(&svg, 16, 16);
    assert_unavailable(decode_validated(&stage_request(&spec).unwrap(), NotSandboxed).err());
    glib::MainContext::new().block_on(async {
        let staged = stage_request(&spec).unwrap();
        assert_unavailable(SandboxedAnimation::open(staged, NotSandboxed).await.err());
        let staged = stage_request(&spec).unwrap();
        assert_unavailable(
            SandboxedRegionDecoder::open(staged, NotSandboxed)
                .await
                .err(),
        );
    });
    let _ = std::fs::remove_file(svg);
}
