// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The first page of an office document, laid out by LibreOffice, for files that
//! carry no saved preview. It runs confined like [`crate::thumbnailer`], one
//! document at a time, from a profile and font cache made once and mounted under
//! an overlay, so a hostile document changes nothing a later render reads.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use gtk::glib;

use crate::thumbnailer::{
    SANDBOX_SOURCE, SandboxSource, open_source, private_staging, sandboxed_command_with,
};

const SOFFICE: &str = "/usr/bin/soffice";
const FC_CACHE: &str = "/usr/bin/fc-cache";
/// The page LibreOffice writes is the paper's size at screen resolution, about
/// 800 by 1100 for A4. A spreadsheet's used area may be far larger; past this
/// it is refused before anything is decoded.
const MAX_PAGE_SIDE: u32 = 8192;
/// Where the template's two directories appear inside the sandbox.
const PROFILE: &str = "/tmp/profile";
const FONT_CACHE: &str = "/tmp/fontconfig";

/// Why there is no page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfficePageFailure {
    /// LibreOffice ran and could not draw this file.
    Document,
    /// Nothing is known about the file: LibreOffice is missing or could not
    /// start, the render ran out of time or was cancelled.
    Unavailable,
}

/// Whether `mime` is a document LibreOffice opens.
#[must_use]
pub fn draws(mime: &str) -> bool {
    const OFFICE: &[&str] = &[
        "application/vnd.oasis.opendocument.",
        "application/vnd.openxmlformats-officedocument.",
        "application/msword",
        "application/vnd.ms-excel",
        "application/vnd.ms-powerpoint",
        "application/rtf",
    ];
    OFFICE.iter().any(|prefix| mime.starts_with(prefix))
}

/// Whether LibreOffice is installed. Asked once per process.
#[must_use]
pub fn installed() -> bool {
    static INSTALLED: OnceLock<bool> = OnceLock::new();
    *INSTALLED.get_or_init(|| Path::new(SOFFICE).is_file())
}

/// Draw the first page of `path` within `bound` pixels.
///
/// Blocking, for up to the thumbnailers' deadline, and one render at a time
/// across the process: a second caller waits for the first.
///
/// # Errors
///
/// [`OfficePageFailure::Document`] when LibreOffice drew nothing from this
/// file, [`OfficePageFailure::Unavailable`] when it could not be asked.
pub fn render(
    path: &Path,
    bound: i32,
    cancellation: &AtomicBool,
) -> Result<crate::BigRgbaImage, OfficePageFailure> {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let unavailable = OfficePageFailure::Unavailable;
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    if cancellation.load(Ordering::Acquire) {
        return Err(unavailable);
    }
    let template = template().ok_or(unavailable)?;
    let source = open_source(path).ok_or(unavailable)?;
    let staging = private_staging().ok_or(unavailable)?;
    let output = sandboxed_command_with(
        &soffice(&["--convert-to", "png", "--outdir", "/tmp", SANDBOX_SOURCE]),
        SandboxSource::Descriptor(source),
        staging.path(),
        &overlay_mounts(template),
    )
    .run_cancellable(cancellation)
    .map_err(|_| unavailable)?;
    // LibreOffice says 0 also when it could not load the file; the page it
    // wrote is the answer.
    let page = staging.path().join("source.png");
    if !output.status.success() || !page.is_file() {
        return Err(OfficePageFailure::Document);
    }
    read_page(&page, bound).ok_or(OfficePageFailure::Document)
}

/// LibreOffice's argv with `arguments` after the options every run shares.
fn soffice<'a>(arguments: &[&'a str]) -> Vec<&'a str> {
    let mut argv = vec![
        SOFFICE,
        "--headless",
        "--norestore",
        "--nologo",
        "--nodefault",
        "--nolockcheck",
        "-env:UserInstallation=file:///tmp/profile",
    ];
    argv.extend_from_slice(arguments);
    argv
}

/// The template's profile and font cache, each under a layer that takes the
/// render's writes and is gone when the sandbox ends.
fn overlay_mounts(template: &Path) -> Vec<std::ffi::OsString> {
    let mut mounts = Vec::new();
    for (directory, inside) in [("profile", PROFILE), ("fontconfig", FONT_CACHE)] {
        mounts.push("--overlay-src".into());
        mounts.push(template.join(directory).into_os_string());
        mounts.push("--tmp-overlay".into());
        mounts.push(inside.into());
    }
    mounts
}

/// The page LibreOffice wrote, refused past [`MAX_PAGE_SIDE`], decoded in the
/// image sandbox with no in-process fallback, and scaled to fit `bound`. The
/// page came out of a process that read an untrusted file, and the sandbox
/// costs little against LibreOffice's own run.
fn read_page(page: &Path, bound: i32) -> Option<crate::BigRgbaImage> {
    let file = big_os_kit::file_io::open_regular_no_follow(page).ok()?;
    // The header alone: the rest is for the sandboxed decoder.
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    let header = decoder.read_header_info().ok()?;
    if header.width > MAX_PAGE_SIDE || header.height > MAX_PAGE_SIDE {
        return None;
    }
    let image =
        crate::decode_rgba_sandboxed(&crate::BigImageSpec::thumbnail(page, bound, bound)).ok()?;
    // The bound is a hint the PNG decoder does not take.
    let (width, height) = crate::fit_within((image.width, image.height), (bound, bound));
    if (width, height) == (image.width, image.height) {
        return Some(image);
    }
    crate::resize_rgba(&image, width, height).ok()
}

/// The profile and font cache every render starts from, made on first use.
///
/// Kept in the user's cache under a name for the installed LibreOffice, so
/// an upgrade makes a new one and the old is removed. Built by LibreOffice
/// and fontconfig with no document open, then proved by starting LibreOffice
/// over it the way a render does: where the kernel refuses an unprivileged
/// overlay, no render is attempted and no file is marked as undrawable.
fn template() -> Option<&'static Path> {
    static TEMPLATE: OnceLock<Option<PathBuf>> = OnceLock::new();
    TEMPLATE.get_or_init(build_template).as_deref()
}

fn build_template() -> Option<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    let program = std::fs::canonicalize(SOFFICE).ok()?;
    let metadata = std::fs::metadata(program.with_file_name("soffice.bin"))
        .or_else(|_| std::fs::metadata(&program))
        .ok()?;
    let name = format!("libreoffice-{:x}-{:x}", metadata.mtime(), metadata.size());
    let root = glib::user_cache_dir().join("big-image");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)
        .ok()?;
    let template = root.join(&name);
    if !template.is_dir() {
        let building = root.join(format!("{name}.{}", glib::uuid_string_random()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&building)
            .ok()?;
        let built = fill_template(&building).is_some()
            && big_os_kit::file_io::rename(&building, &template).is_ok();
        if !built {
            let _ = std::fs::remove_dir_all(&building);
        }
        // Another process may have published the same template first.
        if !template.is_dir() {
            return None;
        }
    }
    // Templates of LibreOffice versions that are not installed.
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let other = entry.file_name();
            let other = other.to_string_lossy();
            if other.starts_with("libreoffice-") && !other.starts_with(&name) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    starts_over(&template).then_some(template)
}

/// Make the font cache and the profile in `directory`, from no document.
fn fill_template(directory: &Path) -> Option<()> {
    // No document is opened: the sandbox's file is a harmless stand-in.
    let stand_in = SandboxSource::Path(Path::new("/usr/bin/true"));
    if Path::new(FC_CACHE).is_file() {
        let _ = sandboxed_command_with::<_, &str>(&[FC_CACHE], stand_in, directory, &[]).run();
    }
    let stand_in = SandboxSource::Path(Path::new("/usr/bin/true"));
    let status = sandboxed_command_with::<_, &str>(
        &soffice(&["--terminate_after_init"]),
        stand_in,
        directory,
        &[],
    )
    .run()
    .ok()?
    .status;
    // A machine without fontconfig's tool still needs the directory for the
    // overlay; LibreOffice then builds the cache in each render's layer.
    std::fs::create_dir_all(directory.join("fontconfig")).ok()?;
    (status.success() && directory.join("profile").is_dir()).then_some(())
}

/// Whether LibreOffice starts over `template` as a render would.
fn starts_over(template: &Path) -> bool {
    let Some(staging) = private_staging() else {
        return false;
    };
    sandboxed_command_with(
        &soffice(&["--terminate_after_init"]),
        SandboxSource::Path(Path::new("/usr/bin/true")),
        staging.path(),
        &overlay_mounts(template),
    )
    .run()
    .is_ok_and(|output| output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_page_is_refused() {
        assert!(crate::thumbnailer::tests::refuses_fifo(|path| {
            read_page(path, 128).is_some()
        }));
    }

    #[test]
    fn claims_only_office_types() {
        assert!(draws("application/vnd.oasis.opendocument.text"));
        assert!(draws(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        ));
        assert!(draws("application/msword"));
        assert!(!draws("application/pdf"));
        assert!(!draws("image/png"));
    }

    /// The template is laid under the render read-only: what a document does
    /// to the profile lands in a layer that goes with the sandbox.
    #[test]
    fn template_is_overlay_source_only() {
        let mounts = overlay_mounts(Path::new("/cache/libreoffice-1"));
        let mounts: Vec<_> = mounts.iter().map(|m| m.to_string_lossy()).collect();
        assert_eq!(
            mounts,
            [
                "--overlay-src",
                "/cache/libreoffice-1/profile",
                "--tmp-overlay",
                PROFILE,
                "--overlay-src",
                "/cache/libreoffice-1/fontconfig",
                "--tmp-overlay",
                FONT_CACHE,
            ]
        );
    }

    /// A real document, when LibreOffice and the sandbox are here.
    #[test]
    fn pictureless_document_uses_first_page() {
        if !installed() || !crate::thumbnailer::sandbox_works() {
            return;
        }
        let folder = std::env::temp_dir().join(format!("big-image-office-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let document = folder.join("page.fodt");
        std::fs::write(
            &document,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
 xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
 office:version="1.3" office:mimetype="application/vnd.oasis.opendocument.text">
 <office:body><office:text><text:p>Hello</text:p></office:text></office:body>
</office:document>"#,
        )
        .unwrap();
        let page = render(&document, 128, &AtomicBool::new(false));
        std::fs::remove_dir_all(&folder).unwrap();
        let page = page.expect("a first page");
        assert!(
            page.width <= 128 && page.height <= 128,
            "{}x{}",
            page.width,
            page.height
        );
        assert!(page.height > page.width, "a portrait page");
    }
}
