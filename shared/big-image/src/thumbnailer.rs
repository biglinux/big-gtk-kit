// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Thumbnails drawn by the programs that install a `.thumbnailer` entry for a
//! type. Those commands parse untrusted files, so each runs under `bwrap` with no
//! network, home or environment, the file bound read-only at a fixed path and a
//! private directory for the PNG, which is then decoded as any untrusted image.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use big_os_kit::confinement::BWRAP;
use big_os_kit::subprocess::{BigSubprocessOutputMode, BigSubprocessSpec};
use gtk::glib;

/// Where the file being drawn appears inside the sandbox: the real path never
/// reaches the command, so a name cannot carry meaning to it.
pub(crate) const SANDBOX_SOURCE: &str = "/tmp/source";
const SANDBOX_OUTPUT: &str = "/tmp/thumbnail.png";
/// A thumbnailer that has not answered by now is not going to.
const DEADLINE: Duration = Duration::from_secs(20);
/// Ceiling on any one file the command writes, as `RLIMIT_FSIZE`.
///
/// Not only the PNG: glycin's loader hands the decoded frame to the
/// thumbnailer through a memfd, and the limit applies to that too. At 16 MiB a
/// 4K AVIF's frame was cut short, the loader died with "zbus i/o error", and no
/// AVIF or HEIC larger than about 2K ever got a thumbnail. 512 MiB carries an
/// 11K-square RGBA frame; the PNG that comes back is still checked against the
/// size asked for before it is read.
const MAX_OUTPUT_BYTES: u64 = 512 * 1024 * 1024;
/// Ceiling on the address space the command may claim.
const MAX_ADDRESS_SPACE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// One `.thumbnailer` entry: the types it claims and the command it offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thumbnailer {
    mime_types: Vec<String>,
    exec: Vec<String>,
}

impl Thumbnailer {
    /// Read one entry, keeping only what can actually run.
    fn parse(text: &str) -> Option<Self> {
        let file = glib::KeyFile::new();
        file.load_from_data(text, glib::KeyFileFlags::NONE).ok()?;
        const GROUP: &str = "Thumbnailer Entry";
        if let Ok(try_exec) = file.string(GROUP, "TryExec")
            && !program_exists(try_exec.as_str())
        {
            return None;
        }
        let exec: Vec<String> = glib::shell_parse_argv(file.string(GROUP, "Exec").ok()?.as_str())
            .ok()?
            .iter()
            .filter_map(|token| token.to_str().map(str::to_owned))
            .collect();
        let program = exec.first()?;
        if !program_exists(program) {
            return None;
        }
        let mime_types: Vec<String> = file
            .string(GROUP, "MimeType")
            .ok()?
            .split(';')
            .filter(|mime| !mime.is_empty())
            .map(str::to_owned)
            .collect();
        (!mime_types.is_empty()).then_some(Self { exec, mime_types })
    }

    fn covers(&self, mime: &str) -> bool {
        self.mime_types.iter().any(|claimed| claimed == mime)
    }

    /// The command line for one file, with the standard's placeholders filled.
    ///
    /// `%i` and `%u` name the file inside the sandbox, never on the real
    /// filesystem: the command has no business knowing where it came from.
    fn command(&self, size: i32) -> Vec<String> {
        self.exec
            .iter()
            .map(|token| {
                token
                    .replace("%i", SANDBOX_SOURCE)
                    .replace("%u", &format!("file://{SANDBOX_SOURCE}"))
                    .replace("%o", SANDBOX_OUTPUT)
                    .replace("%s", &size.to_string())
            })
            .collect()
    }
}

/// The thumbnailers this desktop offers, read once.
///
/// Installing a program that adds one takes effect on the next launch, which
/// is how every file manager on this desktop behaves.
pub fn registry() -> &'static [Thumbnailer] {
    static REGISTRY: std::sync::OnceLock<Vec<Thumbnailer>> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| read_registry(&registry_directories()))
}

/// Whether the installed image loaders decode `mime` themselves.
///
/// Read from glycin's own `conf.d` lists, in every data directory: a picture
/// type they do not claim (DjVu, Photoshop, a camera's raw format) goes to a
/// thumbnailer instead of failing in the decoder and being remembered as a
/// file with no picture.
#[must_use]
pub fn image_decoder_claims(mime: &str) -> bool {
    image_decoder_types().iter().any(|claimed| claimed == mime)
}

fn image_decoder_types() -> &'static [String] {
    static CLAIMED: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    CLAIMED.get_or_init(|| {
        let mut claimed = Vec::new();
        for base in glib::system_data_dirs() {
            let Ok(versions) = std::fs::read_dir(base.join("glycin-loaders")) else {
                continue;
            };
            for version in versions.flatten() {
                let Ok(lists) = std::fs::read_dir(version.path().join("conf.d")) else {
                    continue;
                };
                for list in lists.flatten() {
                    let Ok(text) = std::fs::read_to_string(list.path()) else {
                        continue;
                    };
                    claimed.extend(text.lines().filter_map(|line| {
                        line.strip_prefix("[loader:")?
                            .strip_suffix(']')
                            .map(str::to_owned)
                    }));
                }
            }
        }
        claimed.sort();
        claimed.dedup();
        claimed
    })
}

/// A short fingerprint of what the installed thumbnailers and image loaders
/// can draw.
///
/// A cache that remembers "this file cannot be drawn" has to forget it when
/// that stops being true: installing a font or e-book thumbnailer changes this
/// value, and failures recorded under another value are not believed.
#[must_use]
pub fn registry_signature() -> String {
    let mut claims: Vec<String> = registry()
        .iter()
        .map(|entry| format!("{}={}", entry.exec.join(" "), entry.mime_types.join(";")))
        .collect();
    claims.extend(
        image_decoder_types()
            .iter()
            .map(|mime| format!("glycin={mime}")),
    );
    claims.sort();
    glib::compute_checksum_for_string(glib::ChecksumType::Md5, claims.join("\n"))
        .map(|digest| digest[..8].to_owned())
        .unwrap_or_default()
}

/// The registry's own copy of `mime`, when some installed program draws it.
///
/// The borrowed string outlives the call because the registry is read once and
/// kept: that is what lets a thumbnail key carry the type without allocating
/// per entry in a folder of ten thousand.
#[must_use]
pub fn drawable_mime(mime: &str) -> Option<&'static str> {
    registry()
        .iter()
        .find(|entry| entry.covers(mime))?
        .mime_types
        .iter()
        .find(|claimed| *claimed == mime)
        .map(String::as_str)
}

/// Draw `path` at `bound` pixels through the program that claims its type.
///
/// Blocking: the command runs to completion and the PNG is read back. Belongs
/// on a worker, like the video frame extractor beside it.
#[must_use]
pub fn render(
    path: &Path,
    mime: &str,
    bound: i32,
    cancellation: &AtomicBool,
) -> Option<crate::BigRgbaImage> {
    if cancellation.load(std::sync::atomic::Ordering::Acquire) {
        return None;
    }
    let staging = render_into_staging(path, mime, bound, cancellation)?;
    read_bounded_png(&staging.path().join("thumbnail.png"), bound)
}

/// Draw `path` as [`render`] does, and hand back the picture as a PNG.
///
/// For a caller that keeps the picture as a file, a cache of thumbnails that
/// GTK's own loader reads back without the sandbox. The PNG the program wrote
/// was chosen by a parser of an untrusted file, so it is decoded in the image
/// sandbox and encoded again here: what reaches the cache is bytes this
/// process wrote.
#[must_use]
pub fn render_png(
    path: &Path,
    mime: &str,
    bound: i32,
    cancellation: &AtomicBool,
) -> Option<Vec<u8>> {
    crate::png_bytes(&render(path, mime, bound, cancellation)?)
}

fn render_into_staging(
    path: &Path,
    mime: &str,
    bound: i32,
    cancellation: &AtomicBool,
) -> Option<Staging> {
    if cancellation.load(std::sync::atomic::Ordering::Acquire) {
        return None;
    }
    let entry = registry().iter().find(|entry| entry.covers(mime))?;
    let source = open_source(path)?;
    let staging = private_staging()?;
    let status = sandboxed_command(
        &entry.command(bound),
        SandboxSource::Descriptor(source),
        staging.path(),
    )
    .run_cancellable(cancellation)
    .ok()?;
    status.status.success().then_some(staging)
}

/// The confined command line for one render.
///
/// Split out so a test can read the argv without running anything: what this
/// function decides (which paths the command can see, and under which limits)
/// is the whole security contract of the feature.
pub(crate) fn sandboxed_command<S: AsRef<std::ffi::OsStr>>(
    command: &[S],
    source: SandboxSource<'_>,
    staging: &Path,
) -> BigSubprocessSpec {
    sandboxed_command_with::<S, &str>(command, source, staging, &[])
}

/// [`sandboxed_command`] with `mounts`, more bubblewrap mount arguments, laid
/// over the private `/tmp` after the file being drawn.
pub(crate) fn sandboxed_command_with<S: AsRef<std::ffi::OsStr>, M: AsRef<std::ffi::OsStr>>(
    command: &[S],
    source: SandboxSource<'_>,
    staging: &Path,
    mounts: &[M],
) -> BigSubprocessSpec {
    let mut spec = BigSubprocessSpec::builder()
        .program(BWRAP)
        // The system it needs to run, and nothing else. No home, no /run, no
        // /var: a parser that falls to a hostile document finds nothing of the
        // person's to read, and no way to send it out.
        .args(big_os_kit::confinement::system_only_namespace())
        .args([
            // Fonts: a document thumbnailer draws text, and without these it
            // answers with an empty page instead of a preview.
            "--ro-bind-try",
            "/etc/fonts",
            "/etc/fonts",
            "--ro-bind-try",
            "/usr/share/fonts",
            "/usr/share/fonts",
            "--bind",
        ])
        // The staging directory comes first and the file second: bwrap applies
        // the binds in order, so mounting /tmp after the source would bury it
        // and the command would open nothing.
        .arg(staging.as_os_str())
        .arg("/tmp");
    spec = match source {
        SandboxSource::Path(path) => spec.arg("--ro-bind").arg(path.as_os_str()),
        SandboxSource::Descriptor(file) => {
            let number = std::os::fd::AsRawFd::as_raw_fd(&file).to_string();
            spec.arg("--ro-bind-fd").arg(number).inherit_fd(file.into())
        }
    };
    spec = spec.arg(SANDBOX_SOURCE);
    for mount in mounts {
        spec = spec.arg(mount.as_ref());
    }
    spec = spec.arg("--chdir").arg("/tmp").arg("--");
    for token in command {
        spec = spec.arg(token.as_ref());
    }
    spec.env_inherit(false)
        .env("HOME", "/tmp")
        .env("XDG_CACHE_HOME", "/tmp")
        .stdout(BigSubprocessOutputMode::Null)
        .stderr(BigSubprocessOutputMode::Null)
        .timeout(DEADLINE)
        .unix_process_group(true)
        .max_file_size_bytes(MAX_OUTPUT_BYTES)
        .max_address_space_bytes(MAX_ADDRESS_SPACE_BYTES)
        .nice(crate::THUMBNAIL_NICE)
        .allow_list([BWRAP])
        .build()
}

/// Open the plain file `path` names, through any links on the way, for
/// [`SandboxSource::Descriptor`].
pub(crate) fn open_source(path: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let source = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(std::fs::canonicalize(path).ok()?)
        .ok()?;
    source.metadata().ok()?.is_file().then_some(source)
}

/// The file a confined command reads, mounted read-only at [`SANDBOX_SOURCE`].
pub(crate) enum SandboxSource<'a> {
    /// By path, resolved by bubblewrap when it starts.
    Path(&'a Path),
    /// An open file, mounted as it is: nothing can be swapped in between the
    /// check that it is a plain file and the mount.
    Descriptor(std::fs::File),
}

/// Whether `sandboxed_command` can run here at all.
///
/// Bubblewrap needs unprivileged user namespaces, and some kernels and
/// containers turn them off. Asked once per process, by running `true` in the
/// same confinement a render gets; the answer does not change while it runs.
/// Blocking the first time: it starts bubblewrap.
#[must_use]
pub fn sandbox_works() -> bool {
    static WORKS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WORKS.get_or_init(|| {
        let Some(staging) = private_staging() else {
            return false;
        };
        sandboxed_command(
            &["/usr/bin/true"],
            SandboxSource::Path(Path::new("/usr/bin/true")),
            staging.path(),
        )
        .run()
        .is_ok_and(|output| output.status.success())
    })
}

/// Whether the PNG at `path` is no larger than a thumbnail for `bound` can be.
fn png_within(path: &Path, bound: i32) -> Option<()> {
    let limit = u32::try_from(bound.saturating_mul(4).max(1024)).unwrap_or(u32::MAX);
    let file = big_os_kit::file_io::open_regular_no_follow(path).ok()?;
    let reader = png::Decoder::new(std::io::BufReader::new(file))
        .read_info()
        .ok()?;
    let png_info = reader.info();
    (png_info.width <= limit && png_info.height <= limit).then_some(())
}

/// Read a PNG a thumbnailer wrote, refusing one bigger than it was asked for.
///
/// The command is confined but its answer is still input: a PNG declaring
/// enormous dimensions would have us allocate for it before anything looks at
/// the pixels, and its body is decoded in the image sandbox with no in-process
/// fallback.
fn read_bounded_png(path: &Path, bound: i32) -> Option<crate::BigRgbaImage> {
    png_within(path, bound)?;
    crate::decode_rgba_sandboxed(&crate::BigImageSpec::thumbnail(path, bound, bound)).ok()
}

/// A directory only this process can write, removed when the render ends.
pub(crate) fn private_staging() -> Option<Staging> {
    let root = glib::user_runtime_dir().join("big-image-thumbnailers");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)
        .ok()?;
    let path = root.join(glib::uuid_string_random());
    std::fs::DirBuilder::new().mode(0o700).create(&path).ok()?;
    Some(Staging { path })
}

pub(crate) struct Staging {
    path: PathBuf,
}

impl Staging {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn registry_directories() -> Vec<PathBuf> {
    let mut directories = vec![glib::user_data_dir().join("thumbnailers")];
    directories.extend(
        glib::system_data_dirs()
            .into_iter()
            .map(|base| base.join("thumbnailers")),
    );
    directories
}

fn read_registry(directories: &[PathBuf]) -> Vec<Thumbnailer> {
    let mut entries = Vec::new();
    for directory in directories {
        let Ok(listing) = std::fs::read_dir(directory) else {
            continue;
        };
        for file in listing.flatten() {
            let path = file.path();
            if path
                .extension()
                .is_none_or(|suffix| suffix != "thumbnailer")
            {
                continue;
            }
            if let Ok(text) = big_os_kit::file_io::read_to_string(&path)
                && let Some(entry) = Thumbnailer::parse(&text)
            {
                entries.push(entry);
            }
        }
    }
    entries
}

fn program_exists(program: &str) -> bool {
    if program.starts_with('/') {
        return Path::new(program).is_file();
    }
    big_os_kit::subprocess::program_on_path(program)
}

use std::os::unix::fs::DirBuilderExt as _;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const PAPERS: &str = "[Thumbnailer Entry]\n\
        TryExec=/usr/bin/true\n\
        Exec=/usr/bin/true -s %s %u %o\n\
        MimeType=application/pdf;image/vnd.djvu;\n";

    #[test]
    fn entry_fills_its_placeholders() {
        let entry = Thumbnailer::parse(PAPERS).expect("entry parses");
        assert!(entry.covers("application/pdf"));
        assert!(entry.covers("image/vnd.djvu"));
        assert!(!entry.covers("application/pdf-x"));
        assert_eq!(
            entry.command(256),
            [
                "/usr/bin/true",
                "-s",
                "256",
                "file:///tmp/source",
                "/tmp/thumbnail.png",
            ]
        );
    }

    /// A command that is not installed is not an offer: keeping it would make
    /// every file of that type wait for a process that cannot start.
    #[test]
    fn entry_without_program_is_dropped() {
        let missing = PAPERS.replace("/usr/bin/true", "/usr/bin/does-not-exist-here");
        assert_eq!(Thumbnailer::parse(&missing), None);
        assert_eq!(
            Thumbnailer::parse("[Thumbnailer Entry]\nExec=/usr/bin/true\n"),
            None
        );
    }

    /// The sandbox is the whole security contract, so it is asserted rather
    /// than described: the file arrives read-only under a fixed name, the only
    /// writable place is the staging directory, the environment is empty and
    /// the limits are set.
    #[test]
    fn the_command_runs_confined() {
        let entry = Thumbnailer::parse(PAPERS).expect("entry parses");
        let spec = sandboxed_command(
            &entry.command(128),
            SandboxSource::Path(Path::new("/etc/os-release")),
            Path::new("/run/user/1000/bigfiles-thumbnailers/abc"),
        );
        let resolved = spec.resolved();
        assert_eq!(resolved.program, BWRAP);
        assert!(!resolved.env_inherit);
        assert_eq!(resolved.timeout_ms, DEADLINE.as_millis() as u64);
        let argv = resolved.argv.join(" ");
        assert!(argv.contains("--unshare-all"), "{argv}");
        assert!(argv.contains("--die-with-parent"), "{argv}");
        assert!(
            argv.contains("--ro-bind /etc/os-release /tmp/source"),
            "{argv}"
        );
        assert!(
            argv.contains("--bind /run/user/1000/bigfiles-thumbnailers/abc /tmp"),
            "{argv}"
        );
        // /tmp mounted after the file would cover it, and every document would
        // fail to open.
        assert!(
            argv.find("--bind /run/user/1000/bigfiles-thumbnailers/abc /tmp")
                < argv.find("--ro-bind /etc/os-release /tmp/source"),
            "{argv}"
        );
        // The person's home is never in the sandbox, and neither is the whole
        // filesystem: those are the two binds that would turn a parser bug
        // into a way to read everything they own.
        assert!(!argv.contains("--ro-bind / /"), "{argv}");
        assert!(!argv.contains("--bind /home"), "{argv}");
    }

    #[test]
    fn sandboxed_command_runs_niced() {
        if !sandbox_works() {
            eprintln!("skip: bubblewrap cannot run here");
            return;
        }
        let staging = private_staging().expect("staging directory");
        let output = sandboxed_command(
            &["/usr/bin/cp", "/proc/self/stat", "/tmp/stat"],
            SandboxSource::Path(Path::new("/usr/bin/cp")),
            staging.path(),
        )
        .run()
        .expect("the command runs");
        assert!(output.status.success());
        let stat = std::fs::read_to_string(staging.path().join("stat")).expect("stat copied");
        // Field 19 of proc_pid_stat(5); the fields after the name start at 3.
        let after_name = &stat[stat.rfind(')').expect("stat has a name") + 2..];
        let nice = after_name.split(' ').nth(19 - 3).expect("nice field");
        assert_eq!(nice, crate::THUMBNAIL_NICE.to_string());
    }

    #[test]
    fn open_source_follows_links_skips_folders() {
        let dir = std::env::temp_dir().join(format!("big-image-source-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("test directory");
        let file = dir.join("document.txt");
        std::fs::write(&file, b"text").expect("document");
        let link = dir.join("link.txt");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&file, &link).expect("link");

        assert!(open_source(&link).is_some());
        assert!(open_source(&dir).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The output of the confined program is still a file written by something
    /// this process did not author. A thumbnail of a few hundred pixels that
    /// arrives declaring tens of thousands is either broken or hostile, and
    /// decoding it would allocate for its declared size, not its real one.
    #[test]
    fn oversized_output_is_refused() {
        let dir = std::env::temp_dir().join(format!("bigfiles-thumbnailer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("test directory");
        let file = dir.join("huge.png");
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, 4096, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        writer
            .write_image_data(&vec![0; 4096 * 4])
            .expect("png data");
        writer.finish().expect("png finish");
        std::fs::write(&file, &bytes).expect("write fixture");

        assert!(read_bounded_png(&file, 128).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Whether `read` gives up on a FIFO left where a confined program's
    /// answer belongs. Nothing writes to it, so a blocking open never returns.
    pub(crate) fn refuses_fifo(read: fn(&Path) -> bool) -> bool {
        use std::os::unix::ffi::OsStrExt as _;
        let dir =
            std::env::temp_dir().join(format!("big-image-fifo-{}", glib::uuid_string_random()));
        std::fs::create_dir_all(&dir).expect("test directory");
        let fifo = dir.join("answer.png");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("path");
        // SAFETY: `name` is a live NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || sender.send(read(&fifo)));
        let refused = receiver.recv_timeout(Duration::from_secs(5)) == Ok(false);
        let _ = std::fs::remove_dir_all(&dir);
        refused
    }

    #[test]
    fn fifo_output_is_refused() {
        assert!(refuses_fifo(|path| read_bounded_png(path, 128).is_some()));
    }
}
