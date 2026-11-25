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
