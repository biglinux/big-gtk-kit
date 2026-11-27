// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Themed and file icons drawn at the size they are shown. Every icon file is
//! decoded by glycin in its sandbox and cached in memory and on disk for the
//! whole suite: GTK decodes PNG, JPEG and TIFF in this process, and lays out an
//! SVG icon's `<text>` with Pango on a worker thread, which is not thread-safe.
//! GTK draws only symbolic theme icons and pictures compiled into a program.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

/// Sandboxed decodes allowed at once: for a large icon set, more only adds
/// memory and fewer adds wall clock.
const MAX_IN_FLIGHT: usize = 8;

/// Ceilings on the speculative decode queue, so a burst of icon requests cannot
/// grow it without bound. `MAX_IN_FLIGHT` limits concurrency, not admission:
/// without these, `PENDING` and the retained SVG bytes in it, plus `WAITERS`,
/// climb with every queued request.
const MAX_PENDING_JOBS: usize = 256;
const MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;
/// Pixel ceiling for a rasterised SVG side. The height comes from the icon size,
/// but the width is the natural ratio times that height, so a pathologically
/// wide SVG could ask for an enormous texture off a tiny file. Cap it.
const MAX_SVG_OUTPUT_SIDE: u32 = 1024;

/// Hard cap on textures kept in this process beyond the ones widgets still show.
const MEMORY_CACHE_CAP: usize = 512;
/// Byte ceiling for the icons kept after their widgets are gone: a view of
/// every installed application fills any larger one and holds it for the
/// session. An icon still on screen is found through `LIVE` whatever this says.
const MEMORY_CACHE_BYTES: usize = 4 * 1024 * 1024;

/// The largest icon file read at all. Icons are kilobytes; a megabyte-scale
/// "icon" is not one.
const MAX_ICON_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// On-disk entry layout version. Bump when the header or pixel format changes.
const DISK_FORMAT: &str = "rgba-premul-1";
/// Entries unchanged this long are removed: a key names its source's size and
/// time, so one whose source changed is never read again, and a read marks an
/// entry older than half of this again.
const DISK_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// How often the programs sharing the cache prune it, between them.
const DISK_PRUNE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// The file in the cache folder whose modification time is the last pruning.
const DISK_PRUNE_STAMP: &str = "pruned";

/// Who waits for a decode: `None` when the sandbox could not draw it.
type Waiter = Box<dyn FnOnce(Option<gdk::Texture>)>;

struct PendingSvg {
    key: String,
    bytes: glib::Bytes,
    height_px: i32,
}

thread_local! {
    /// Rendered textures by key, with a recency tick for LRU eviction.
    static MEMORY: RefCell<HashMap<String, (gdk::Texture, u64)>> = RefCell::new(HashMap::new());
    /// Every texture handed out, weakly: one still drawn by some widget is given
    /// again instead of being copied from the disk cache a second time.
    static LIVE: RefCell<HashMap<String, glib::WeakRef<gdk::Texture>>> =
        RefCell::new(HashMap::new());
    static TICK: Cell<u64> = const { Cell::new(0) };
    /// Who wants each texture that is still being decoded, by key. A second
    /// request for the same key while the first is in flight joins it here.
    static WAITERS: RefCell<HashMap<String, Vec<Waiter>>> = RefCell::new(HashMap::new());
    /// Decodes not yet started, in request order.
    static PENDING: RefCell<VecDeque<PendingSvg>> = const { RefCell::new(VecDeque::new()) };
    /// Bytes of SVG source retained by `PENDING`, kept in step with it.
    static PENDING_BYTES: Cell<usize> = const { Cell::new(0) };
    /// Decodes running right now.
    static IN_FLIGHT: Cell<usize> = const { Cell::new(0) };
    /// Where the theme found each themed icon asked for, by the icon's string
    /// form, size and scale. Forgotten whenever the theme changes.
    static RESOLVED: RefCell<HashMap<(String, i32, i32), Resolved>> = RefCell::new(HashMap::new());
    static WATCHING_THEME: Cell<bool> = const { Cell::new(false) };
    /// Keys the sandbox could not draw, and when it last tried.
    static FAILED: RefCell<HashMap<String, Instant>> = RefCell::new(HashMap::new());
}

/// What the theme answered for one icon.
#[derive(Clone)]
enum Resolved {
    /// GTK draws it: a symbolic theme icon, recolored as it is drawn, or a
    /// picture compiled into a program (GTK's own one for a theme miss).
    Gtk,
    /// A local file for the sandbox, and its cache key.
    File(PathBuf, String),
    /// Nothing the sandbox reads here and GTK must not decode: a remote file,
    /// bytes, a texture.
    Refused,
}

/// Ceiling on remembered theme answers; past it the memo starts over.
const MAX_RESOLVED: usize = 4096;

/// How long a failed decode is believed: a loader the kernel ended under
/// memory pressure fails like a broken file, and the shell runs all session.
const FAILED_FOR: Duration = Duration::from_secs(10 * 60);
/// Ceiling on remembered failures; past it the record starts over.
const MAX_FAILED: usize = 1024;

/// What the icon theme draws `gicon` from at `size`×`scale`; remembered for a
/// themed icon, as a file view asks again for every cell it binds and each
/// answer reads file metadata on the main thread.
fn resolve(display: &gdk::Display, gicon: &gio::Icon, size: i32, scale: i32) -> Resolved {
    let theme = gtk::IconTheme::for_display(display);
    if !WATCHING_THEME.replace(true) {
        theme.connect_changed(|_| RESOLVED.with(|memo| memo.borrow_mut().clear()));
    }
    // Themed icons only: a file icon's file can change under the same name, and
    // its key has to see the new modification time.
    let themed = gicon.downcast_ref::<gio::ThemedIcon>();
    let name = themed
        .and_then(|_| IconExt::to_string(gicon))
        .map(|name| (name.to_string(), size, scale));
    if let Some(name) = &name
        && let Some(answer) = RESOLVED.with(|memo| memo.borrow().get(name).cloned())
    {
        return answer;
    }
    // GTK cannot draw a file icon without a local path (it logs criticals).
    if gicon
        .downcast_ref::<gio::FileIcon>()
        .is_some_and(|icon| icon.file().path().is_none())
    {
        return Resolved::Refused;
    }
    // Only the first name: GLib appends a `-symbolic` twin of every name.
    let asks_symbolic = themed
        .and_then(|themed| themed.names().first().cloned())
        .is_some_and(|name| name.ends_with("-symbolic"));
    let flags = if asks_symbolic {
        gtk::IconLookupFlags::empty()
    } else {
        gtk::IconLookupFlags::FORCE_REGULAR
    };
    let paintable = theme.lookup_by_gicon(gicon, size, scale, gtk::TextDirection::None, flags);
    let answer = match paintable.file() {
        Some(file) => match file.path() {
            // A file icon named like a symbolic one is still a file from anywhere.
            Some(path) if themed.is_some() && symbolic_file(&path) => Resolved::Gtk,
            Some(path) => {
                let key = source_key(&path, (size * scale).max(1), "");
                Resolved::File(path, key)
            }
            None if file.has_uri_scheme("resource") => Resolved::Gtk,
            None => Resolved::Refused,
        },
        None => Resolved::Refused,
    };
    if let Some(name) = name {
        RESOLVED.with(|memo| {
            let mut memo = memo.borrow_mut();
            if memo.len() >= MAX_RESOLVED {
                memo.clear();
            }
            memo.insert(name, answer.clone());
        });
    }
    answer
}

/// Whether GTK recolors the icon file at `path` as it draws it, which it tells
/// by the file name alone (`gtk_icon_paintable_is_symbolic`, deprecated without a
/// successor).
fn symbolic_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(std::ffi::OsStr::as_encoded_bytes)
        .unwrap_or_default();
    [
        &b"-symbolic.svg"[..],
        b"-symbolic-ltr.svg",
        b"-symbolic-rtl.svg",
        b".symbolic.png",
    ]
    .into_iter()
    .any(|suffix| name.ends_with(suffix))
}

/// Whether the sandbox failed to draw `key` within [`FAILED_FOR`].
fn failed_lately(key: &str) -> bool {
    FAILED.with(|failed| {
        failed
            .borrow()
            .get(key)
            .is_some_and(|when| when.elapsed() < FAILED_FOR)
    })
}

/// Note that the sandbox could not draw `key`.
fn record_failure(key: String) {
    FAILED.with(|failed| {
        let mut failed = failed.borrow_mut();
        if failed.len() >= MAX_FAILED {
            failed.clear();
        }
        failed.insert(key, Instant::now());
    });
}

/// What GTK draws for an icon the sandbox could not, or would not, read.
fn missing_icon() -> gio::Icon {
    gio::ThemedIcon::new("image-missing-symbolic").upcast()
}

/// A cache key for `path` drawn `px` tall with `variant` (a tint, or empty).
///
/// The file's length and modification time are part of it, so an icon theme
/// update stops matching the entries of the icon it replaced.
#[must_use]
pub fn source_key(path: &Path, px: i32, variant: &str) -> String {
    let stamp = std::fs::metadata(path)
        .ok()
        .map(|meta| {
            let modified = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_nanos());
            format!("{}:{modified}", meta.len())
        })
        .unwrap_or_default();
    format!("{}|{stamp}|{px}|{variant}", path.display())
}

/// Set `image` from the themed icon `name`, or from the file when `name` is an
/// absolute path, the two things a `.desktop` `Icon=` can be.
pub fn set_icon_name(image: &gtk::Image, name: &str) {
    if name.starts_with('/') {
        set_icon_file(image, Path::new(name));
    } else if name.ends_with("-symbolic") {
        // Stored by name, as GTK itself would: through a `GThemedIcon` the image
        // answered `None` to `icon_name()`, and callers compare what they show.
        image.set_icon_name(Some(name));
    } else {
        set_gicon(image, &gio::ThemedIcon::new(name).upcast());
    }
}

/// Set `image` from the icon or picture file at `path`, at the image's size.
pub fn set_icon_file(image: &gtk::Image, path: &Path) {
    set_gicon(
        image,
        &gio::FileIcon::new(&gio::File::for_path(path)).upcast(),
    );
}

/// Set `image` from a themed, mime-type or file `gicon` at the image's own
/// pixel size.
///
/// The file behind the icon is decoded by the sandbox at that size off the main
/// thread and arrives a moment later (at once when cached); the image is blank
/// meanwhile. GTK draws what [`gicon_texture`] leaves to it. An answer that
/// arrives after the image was set again, as a recycled list row is, is dropped.
pub fn set_gicon(image: &gtk::Image, gicon: &gio::Icon) {
    let size = image.pixel_size().max(16);
    let scale = image.scale_factor().max(1);
    let asked = gdk::Paintable::new_empty(size, size);
    image.set_paintable(Some(&asked));
    let weak = image.downgrade();
    gicon_texture(&image.display(), gicon, size, scale, move |answer| {
        let Some(image) = weak.upgrade() else {
            return;
        };
        if image.paintable().as_ref() != Some(&asked) {
            return;
        }
        match answer {
            Ok(texture) => image.set_paintable(Some(&texture)),
            Err(icon) => image.set_from_gicon(&icon),
        }
    });
}

/// [`gicon_texture`] as a plain function, for a view that takes an icon loader
/// (`big_item_view::IconLoader`).
pub const GICON_TEXTURE: fn(&gdk::Display, &gio::Icon, i32, i32, DeliverIcon) = gicon_texture;

/// Where [`GICON_TEXTURE`] hands the picture.
pub type DeliverIcon = Box<dyn FnOnce(Result<gdk::Texture, gio::Icon>)>;

/// Deliver `gicon` drawn `size`×`scale` pixels, the way [`set_gicon`] draws it, for
/// a surface that takes a texture rather than a `gtk::Image`: a preview tile, a
/// drag icon.
///
/// `Err` carries the icon for GTK to draw instead: `gicon` itself when it is a
/// symbolic theme icon (recolored as it is drawn, which a bitmap cannot be) or a
/// picture compiled into a program, and `image-missing-symbolic` when the
/// sandbox could not decode the file or the icon is not a local file (a remote
/// one, bytes). GTK never gets a file the sandbox refused. `deliver` runs once,
/// on the main thread, at once when cached; a failure is answered at once for
/// ten minutes after it.
///
/// GTK's own loader keeps a raster icon at the size of its file (pixmaps ship
/// 1024-pixel PNGs), and every image embedded in an SVG for the life of the
/// process; the sandbox decodes at the size shown instead.
pub fn gicon_texture(
    display: &gdk::Display,
    gicon: &gio::Icon,
    size: i32,
    scale: i32,
    deliver: impl FnOnce(Result<gdk::Texture, gio::Icon>) + 'static,
) {
    let size = size.max(16);
    let scale = scale.max(1);
    let (path, key) = match resolve(display, gicon, size, scale) {
        Resolved::Gtk => {
            deliver(Err(gicon.clone()));
            return;
        }
        Resolved::Refused => {
            deliver(Err(missing_icon()));
            return;
        }
        Resolved::File(path, key) => (path, key),
    };
    let deliver = move |texture: Option<gdk::Texture>| deliver(texture.ok_or_else(missing_icon));
    let px = (size * scale).max(1);
    if path.extension().and_then(|ext| ext.to_str()) != Some("svg") {
        raster_texture(path, (px, px), key, deliver);
        return;
    }
    if let Some(texture) = memory_hit(&key) {
        deliver(Some(texture));
        return;
    }
    if failed_lately(&key) {
        deliver(None);
        return;
    }
    // Read the cached bitmap (or, on a miss, the SVG) off the main thread, and
    // decode off the main thread too. Opening a menu of many icons must never
    // block the UI reading them from a slow disk: the tile keeps its placeholder
    // until the texture lands a moment later. The rendered bitmap already on disk
    // is authoritative, so it is honored before the SVG is read and decoded
    // again. GTK's own guidance is `spawn_blocking` for this file I/O; the SVG
    // decode already runs in the glycin sandbox.
    glib::spawn_future_local(async move {
        let cache_key = key.clone();
        if let Ok(Some((width, height, stride, pixels))) =
            gio::spawn_blocking(move || read_disk_rgba(&cache_key)).await
        {
            let texture = texture_from_rgba(width, height, stride, pixels);
            memory_put(key, texture.clone());
            deliver(Some(texture));
            return;
        }
        let Ok(Some(bytes)) = gio::spawn_blocking(move || {
            big_os_kit::file_io::read_bounded(&path, MAX_ICON_FILE_BYTES).ok()
        })
        .await
        else {
            record_failure(key);
            deliver(None);
            return;
        };
        svg_answer(key, glib::Bytes::from_owned(bytes), px, deliver);
    });
}

/// Deliver the raster at `path` fitted inside `bound`: from memory at once,
/// else from the disk cache or the sandbox a moment later, `None` when it could
/// not be decoded. `deliver` runs on the main thread either way.
fn raster_texture(
    path: PathBuf,
    bound: (i32, i32),
    key: String,
    deliver: impl FnOnce(Option<gdk::Texture>) + 'static,
) {
    if let Some(texture) = memory_hit(&key) {
        deliver(Some(texture));
        return;
    }
    if failed_lately(&key) {
        deliver(None);
        return;
    }
    if !join_waiters(&key, Box::new(deliver)) {
        return;
    }
    glib::spawn_future_local(async move {
        let cache_key = key.clone();
        let cached = gio::spawn_blocking(move || read_disk_rgba(&cache_key))
            .await
            .ok()
            .flatten();
        let fresh = cached.is_none();
        let pixels = match cached {
            Some(pixels) => Some(pixels),
            None => gio::spawn_blocking(move || reduced_raster(&path, bound))
                .await
                .ok()
                .flatten(),
        };
        let texture = pixels.map(|(width, height, stride, pixels)| {
            texture_from_rgba(width, height, stride, pixels)
        });
        match &texture {
            Some(texture) => {
                if fresh {
                    disk_put(&key, texture);
                }
                memory_put(key.clone(), texture.clone());
            }
            None => record_failure(key.clone()),
        }
        answer_waiters(&key, texture.as_ref());
    });
}

/// Wait for `key` with the decode already asked for, or be the first: `true`
/// when the caller has to start it.
fn join_waiters(key: &str, waiter: Waiter) -> bool {
    WAITERS.with(|waiters| {
        let mut waiters = waiters.borrow_mut();
        let entry = waiters.entry(key.to_owned()).or_default();
        entry.push(waiter);
        entry.len() == 1
    })
}

/// Hand everybody waiting for `key` what its decode gave.
fn answer_waiters(key: &str, texture: Option<&gdk::Texture>) {
    let waiters = WAITERS
        .with(|waiters| waiters.borrow_mut().remove(key))
        .unwrap_or_default();
    for deliver in waiters {
        deliver(texture.cloned());
    }
}

/// Decode the raster at `path` to fit inside `bound`: `(width, height, stride,
/// premultiplied RGBA)`. Worker thread only.
///
/// Through this crate's own sandboxed decoder, whatever the format (PNG, JPEG,
/// XPM, ICO, WebP…), and never gdk-pixbuf: its glycin loader copies every file
/// to `/tmp` before the sandbox sees it. `None` when the sandbox cannot decode
/// it.
fn reduced_raster(path: &Path, bound: (i32, i32)) -> Option<(i32, i32, usize, Vec<u8>)> {
    // Themes link many icons to one file; the decoder opens no link itself.
    let path = crate::followed_link(path.to_path_buf());
    let image =
        crate::decode_rgba_sandboxed(&crate::BigImageSpec::bounded(&path, bound.0, bound.1))
            .ok()?;
    let row = usize::try_from(image.width).ok()? * 4;
    let mut pixels = image.rgba;
    // Straight alpha to premultiplied, which is what the disk cache holds.
    for rgba in pixels.as_chunks_mut::<4>().0 {
        let alpha = u16::from(rgba[3]);
        for channel in &mut rgba[..3] {
            *channel = u8::try_from((u16::from(*channel) * alpha + 127) / 255).ok()?;
        }
    }
    Some((image.width, image.height, row, pixels))
}

/// Render an icon from an absolute file `path` at `px`×`scale` tall and hand it
/// to `deliver`: an SVG drawn by the sandbox at that height, a raster decoded by
/// it to fit, both off the main thread and cached. Nothing is delivered for a
/// file that is not an image.
///
/// Scaled to the height asked for, keeping the shape, and not handed over whole:
/// a texture's size is what a `GtkPicture` asks its parent for, so a wide
/// photograph as a Start button would claim most of the bar for a small picture
/// floating in the middle of it.
pub fn file_texture(
    path: &Path,
    px: i32,
    scale: i32,
    deliver: impl FnOnce(gdk::Texture) + 'static,
) {
    let height = (px * scale.max(1)).max(1);
    let key = source_key(path, height, "");
    if path.extension().and_then(|ext| ext.to_str()) != Some("svg") {
        // As wide as a banner may reasonably be; the height is what was asked.
        let bound = (height.saturating_mul(8), height);
        raster_texture(path.to_path_buf(), bound, key, |texture| {
            if let Some(texture) = texture {
                deliver(texture);
            }
        });
        return;
    }
    if let Some(texture) = memory_hit(&key) {
        deliver(texture);
        return;
    }
    let path = path.to_path_buf();
    glib::spawn_future_local(async move {
        let Ok(Some(bytes)) = gio::spawn_blocking(move || {
            big_os_kit::file_io::read_bounded(&path, MAX_ICON_FILE_BYTES).ok()
        })
        .await
        else {
            return;
        };
        svg_texture(key, glib::Bytes::from_owned(bytes), height, deliver);
    });
}

/// Deliver the SVG compiled into the program at `resource_path`, drawn
/// `height_px` tall by the sandbox and cached like any other icon. Nothing is
/// delivered when the resource is missing.
pub fn resource_texture(
    resource_path: &str,
    height_px: i32,
    deliver: impl FnOnce(gdk::Texture) + 'static,
) {
    let Ok(bytes) = gio::resources_lookup_data(resource_path, gio::ResourceLookupFlags::NONE)
    else {
        return;
    };
    // The bytes stand in for a file's size and time: a new build with a changed
    // picture gets a new entry.
    let digest = glib::compute_checksum_for_bytes(glib::ChecksumType::Sha256, &bytes)
        .map(|digest| digest.to_string())
        .unwrap_or_default();
    let key = format!("resource:{resource_path}|{digest}|{height_px}|");
    svg_texture(key, bytes, height_px, deliver);
}

/// Deliver the PNG at `path` as it is, decoded off the main thread.
///
/// Only for a file this suite encoded itself, already at the size it is shown:
/// a thumbnail cache filled by [`crate::thumbnailer::render_png`] or
/// [`crate::png_bytes`]. It is read by GTK's own PNG loader, without the
/// sandbox, because nobody else chose its bytes. Nothing is delivered when it
/// cannot be read.
#[allow(
    clippy::disallowed_methods,
    reason = "a file this suite wrote itself, at the size it is shown"
)]
pub fn own_png_texture(path: &Path, deliver: impl FnOnce(gdk::Texture) + 'static) {
    let path = path.to_path_buf();
    glib::spawn_future_local(async move {
        // GTK documents this loader as safe to call from a worker thread.
        if let Ok(Some(texture)) =
            gio::spawn_blocking(move || gdk::Texture::from_filename(&path).ok()).await
        {
            deliver(texture);
        }
    });
}

/// Deliver the texture for SVG `bytes` drawn `height_px` tall, as wide as that
/// height makes it: from a cache at once, else from the sandboxed decoder when
/// it is done. `deliver` runs on the main thread either way, and not at all when
/// the sandbox could not draw it.
///
/// `key` names the source (see [`source_key`]); two callers asking for the same
/// key while a decode is in flight share it.
pub fn svg_texture(
    key: String,
    bytes: glib::Bytes,
    height_px: i32,
    deliver: impl FnOnce(gdk::Texture) + 'static,
) {
    svg_answer(key, bytes, height_px, |texture| {
        if let Some(texture) = texture {
            deliver(texture);
        }
    });
}

/// [`svg_texture`], answering `None` when the sandbox could not draw it.
fn svg_answer(
    key: String,
    bytes: glib::Bytes,
    height_px: i32,
    deliver: impl FnOnce(Option<gdk::Texture>) + 'static,
) {
    if let Some(texture) = memory_hit(&key) {
        deliver(Some(texture));
        return;
    }
    if failed_lately(&key) {
        deliver(None);
        return;
    }
    if let Some(texture) = disk_hit(&key) {
        memory_put(key, texture.clone());
        deliver(Some(texture));
        return;
    }
    if !join_waiters(&key, Box::new(deliver)) {
        return;
    }
    // Bound the speculative queue: under a flood of icon requests, refuse to
    // enqueue past the job or byte ceiling rather than growing PENDING and the
    // SVG bytes it holds without limit. The refusal is not remembered, so a
    // later request decodes it.
    let over_budget = PENDING.with(|pending| pending.borrow().len()) >= MAX_PENDING_JOBS
        || PENDING_BYTES.with(Cell::get).saturating_add(bytes.len()) > MAX_PENDING_BYTES;
    if over_budget {
        answer_waiters(&key, None);
        return;
    }
    PENDING_BYTES.with(|total| total.set(total.get().saturating_add(bytes.len())));
    PENDING.with(|pending| {
        pending.borrow_mut().push_back(PendingSvg {
            key,
            bytes,
            height_px,
        });
    });
    start_pending_decodes();
}

fn start_pending_decodes() {
    loop {
        if IN_FLIGHT.with(Cell::get) >= MAX_IN_FLIGHT {
            return;
        }
        let Some(job) = PENDING.with(|pending| pending.borrow_mut().pop_front()) else {
            return;
        };
        PENDING_BYTES.with(|total| total.set(total.get().saturating_sub(job.bytes.len())));
        IN_FLIGHT.with(|count| count.set(count.get() + 1));
        glib::MainContext::default().spawn_local(async move {
            let texture = decode_svg(job.bytes, job.height_px).await;
            IN_FLIGHT.with(|count| count.set(count.get().saturating_sub(1)));
            match &texture {
                Some(texture) => {
                    memory_put(job.key.clone(), texture.clone());
                    disk_put(&job.key, texture);
                }
                None => record_failure(job.key.clone()),
            }
            answer_waiters(&job.key, texture.as_ref());
            start_pending_decodes();
        });
    }
}

/// Rasterise SVG `bytes` in the glycin sandbox at `height_px`, keeping the
/// picture's own proportions.
pub(crate) async fn decode_svg(bytes: glib::Bytes, height_px: i32) -> Option<gdk::Texture> {
    let height = u32::try_from(height_px.max(1)).ok()?;
    crate::glycin_threads();
    let mut loader = glycin::Loader::new_bytes(bytes);
    loader.accepted_memory_formats(glycin::MemoryFormatSelection::R8g8b8a8);
    let image = loader.load().await.ok()?;
    let details = image.details();
    let (natural_w, natural_h) = (details.width(), details.height());
    if natural_w == 0 || natural_h == 0 {
        return None;
    }
    let width = (f64::from(height) * f64::from(natural_w) / f64::from(natural_h)).round();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a rounded positive width; `as` saturates"
    )]
    let width = (width as u32).max(1);
    // A pathologically wide SVG asks for a huge width off a tiny file; cap the
    // requested side so one icon cannot demand an enormous texture.
    let width = width.min(MAX_SVG_OUTPUT_SIDE);
    let height = height.min(MAX_SVG_OUTPUT_SIDE);
    let frame = image
        .specific_frame(glycin::FrameRequest::new().scale(width, height))
        .await
        .ok()?;
    Some(frame.texture())
}

fn tick() -> u64 {
    TICK.with(|tick| {
        let next = tick.get().wrapping_add(1);
        tick.set(next);
        next
    })
}

/// `key`'s texture from this process's cache, with its recency bumped.
#[must_use]
pub fn memory_hit(key: &str) -> Option<gdk::Texture> {
    let recent = MEMORY.with(|cache| {
        cache.borrow_mut().get_mut(key).map(|entry| {
            entry.1 = tick();
            entry.0.clone()
        })
    });
    recent.or_else(|| LIVE.with(|live| live.borrow().get(key).and_then(glib::WeakRef::upgrade)))
}

/// Remember `texture` under `key`, evicting the least recently used at the cap.
pub fn memory_put(key: String, texture: gdk::Texture) {
    let width = usize::try_from(texture.width().max(0)).unwrap_or(0);
    let height = usize::try_from(texture.height().max(0)).unwrap_or(0);
    if width.saturating_mul(height).saturating_mul(4) > MEMORY_CACHE_BYTES {
        // Delivering an icon need not retain it in the cache. Do not exempt a
        // single oversized texture from the advertised byte ceiling.
        return;
    }
    LIVE.with(|live| {
        let mut live = live.borrow_mut();
        // Forget the textures nobody draws any more, now and then rather than on
        // every insert.
        if live.len() >= MEMORY_CACHE_CAP * 2 {
            live.retain(|_, texture| texture.upgrade().is_some());
        }
        live.insert(key.clone(), texture.downgrade());
    });
    let now = tick();
    MEMORY.with(|cache| {
        let mut map = cache.borrow_mut();
        map.insert(key, (texture, now));
        // Evict the least recently used until under both the object and the byte
        // ceiling, keeping at least the entry just inserted.
        while map.len() > MEMORY_CACHE_CAP
            || (map.len() > 1 && memory_cache_bytes(&map) > MEMORY_CACHE_BYTES)
        {
            let Some(lru) = map
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            map.remove(&lru);
        }
    });
}

/// Drop every cached icon texture the process is holding.
///
/// For memory pressure: the textures still shown keep their own reference through
/// the `gtk::Image` that drew them, so this frees only the ones whose widgets are
/// already gone. Nothing repaints, and a later miss re-reads the small icon file.
/// In-flight decodes (`WAITERS`) are left alone so their callbacks still deliver.
pub fn purge_memory_cache() {
    MEMORY.with(|cache| cache.borrow_mut().clear());
    LIVE.with(|live| {
        live.borrow_mut()
            .retain(|_, texture| texture.upgrade().is_some())
    });
}

fn memory_cache_bytes(map: &HashMap<String, (gdk::Texture, u64)>) -> usize {
    map.values()
        .map(|(texture, _)| {
            let width = usize::try_from(texture.width().max(0)).unwrap_or(0);
            let height = usize::try_from(texture.height().max(0)).unwrap_or(0);
            width.saturating_mul(height).saturating_mul(4)
        })
        .sum()
}

/// Where the suite's programs share rendered icons: `$XDG_CACHE_HOME/big-icons/<format>/`.
fn disk_dir() -> PathBuf {
    glib::user_cache_dir().join("big-icons").join(DISK_FORMAT)
}

fn disk_path(key: &str) -> Option<PathBuf> {
    let digest = glib::compute_checksum_for_string(glib::ChecksumType::Sha256, key)?;
    Some(disk_dir().join(format!("{digest}.rgba")))
}

/// The header is two little-endian u32: width, height. Pixels follow, tightly
/// packed premultiplied RGBA, so the byte count is implied and checked.
const HEADER_LEN: usize = 8;

/// Read and validate one cached bitmap. Returns `(width, height, stride,
/// premultiplied RGBA pixels)`. Holds no GTK object, so it is safe to run on a
/// worker thread; the texture is built from the result on the main thread.
fn read_disk_rgba(key: &str) -> Option<(i32, i32, usize, Vec<u8>)> {
    read_disk_entry(&disk_path(key)?)
}

fn read_disk_entry(path: &Path) -> Option<(i32, i32, usize, Vec<u8>)> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).ok()?;
    // A read leaves the time alone on a `noatime` home, so an entry in use
    // is marked again before the prune would take it.
    let written = file.metadata().and_then(|metadata| metadata.modified());
    if written.is_ok_and(|written| written.elapsed().is_ok_and(|age| age >= DISK_MAX_AGE / 2)) {
        let _ = std::fs::File::options()
            .write(true)
            .open(path)
            .and_then(|file| file.set_modified(SystemTime::now()));
    }
    let (header, pixels) = bytes.split_at_checked(HEADER_LEN)?;
    let width = u32::from_le_bytes(header[0..4].try_into().ok()?);
    let height = u32::from_le_bytes(header[4..8].try_into().ok()?);
    let stride = usize::try_from(width).ok()?.checked_mul(4)?;
    let expected = stride.checked_mul(usize::try_from(height).ok()?)?;
    if width == 0 || height == 0 || pixels.len() != expected {
        // Damaged or truncated: throw it away and let the caller render again.
        let _ = std::fs::remove_file(path);
        return None;
    }
    let (width, height) = (i32::try_from(width).ok()?, i32::try_from(height).ok()?);
    Some((width, height, stride, pixels.to_vec()))
}

fn texture_from_rgba(width: i32, height: i32, stride: usize, pixels: Vec<u8>) -> gdk::Texture {
    gdk::MemoryTexture::new(
        width,
        height,
        gdk::MemoryFormat::R8g8b8a8Premultiplied,
        &glib::Bytes::from_owned(pixels),
        stride,
    )
    .upcast()
}

fn disk_hit(key: &str) -> Option<gdk::Texture> {
    let (width, height, stride, pixels) = read_disk_rgba(key)?;
    Some(texture_from_rgba(width, height, stride, pixels))
}

/// Write `texture` for `key`, for the other programs. Best effort: a cache that
/// cannot be written is a cache that is empty, not an error anybody sees.
fn disk_put(key: &str, texture: &gdk::Texture) {
    let Some(path) = disk_path(key) else { return };
    let mut downloader = gdk::TextureDownloader::new(texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8Premultiplied);
    let (pixels, stride) = downloader.download_bytes();
    let (width, height) = (texture.width(), texture.height());
    let (Ok(w), Ok(h)) = (u32::try_from(width), u32::try_from(height)) else {
        return;
    };
    let row = usize::try_from(width).unwrap_or(0) * 4;
    let mut out = Vec::with_capacity(HEADER_LEN + row * usize::try_from(height).unwrap_or(0));
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    // Repack without the row padding the downloader may use.
    for line in pixels
        .chunks(stride)
        .take(usize::try_from(height).unwrap_or(0))
    {
        out.extend_from_slice(&line[..row.min(line.len())]);
    }
    if out.len() != HEADER_LEN + row * usize::try_from(height).unwrap_or(0) {
        return;
    }
    // Off the main loop: a write to the cache directory is small, but a slow
    // home is not the icon's problem. A reader never sees a partial entry, and a
    // failed write only costs the next start a decode.
    gio::spawn_blocking(move || {
        if big_os_kit::storage::atomic_write(&path, &out).is_ok()
            && let Some(directory) = path.parent()
        {
            prune_disk_cache(directory, SystemTime::now());
        }
    });
}

/// Remove the entries of `directory` older than [`DISK_MAX_AGE`], unless one
/// of the programs sharing it did within [`DISK_PRUNE_INTERVAL`] of `now`.
fn prune_disk_cache(directory: &Path, now: SystemTime) {
    // A time ahead of `now` (the clock was set back) is no age at all, so it
    // neither keeps an entry stale nor holds the next pruning off.
    let age = |modified: SystemTime| now.duration_since(modified).ok();
    let stamp = directory.join(DISK_PRUNE_STAMP);
    let pruned = std::fs::metadata(&stamp).and_then(|metadata| metadata.modified());
    if pruned.is_ok_and(|pruned| age(pruned).is_some_and(|age| age < DISK_PRUNE_INTERVAL)) {
        return;
    }
    if std::fs::File::create(&stamp)
        .and_then(|file| file.set_modified(now))
        .is_err()
    {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry.file_name() != DISK_PRUNE_STAMP
            && entry.metadata().is_ok_and(|metadata| {
                metadata.is_file()
                    && metadata
                        .modified()
                        .is_ok_and(|modified| age(modified).is_some_and(|age| age >= DISK_MAX_AGE))
            });
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own under the temporary directory, for icon files.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("big-icon-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A valid 8×8 PNG, encoded by GTK.
    fn png() -> Vec<u8> {
        let pixels = glib::Bytes::from_owned(vec![200u8; 8 * 8 * 4]);
        gdk::MemoryTexture::new(8, 8, gdk::MemoryFormat::R8g8b8a8, &pixels, 8 * 4)
            .save_to_png_bytes()
            .to_vec()
    }

    fn file_icon(path: &Path) -> gio::Icon {
        gio::FileIcon::new(&gio::File::for_path(path)).upcast()
    }

    /// What [`gicon_texture`] answers for `gicon` at 32 pixels, and whether it
    /// answered before returning.
    fn answer(gicon: &gio::Icon) -> (Result<gdk::Texture, gio::Icon>, bool) {
        let display = gdk::Display::default().expect("a display");
        let slot = std::rc::Rc::new(RefCell::new(None));
        let sink = slot.clone();
        gicon_texture(&display, gicon, 32, 1, move |answer| {
            assert!(sink.replace(Some(answer)).is_none(), "answered twice");
        });
        let at_once = slot.borrow().is_some();
        let deadline = Instant::now() + Duration::from_secs(20);
        while slot.borrow().is_none() {
            assert!(Instant::now() < deadline, "no answer for {gicon:?}");
            glib::MainContext::default().iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        (slot.take().expect("an answer"), at_once)
    }

    fn missing(answer: &Result<gdk::Texture, gio::Icon>) -> bool {
        answer
            .as_ref()
            .err()
            .and_then(|icon| icon.downcast_ref::<gio::ThemedIcon>())
            .is_some_and(|icon| {
                icon.names().first().map(glib::GString::as_str) == Some("image-missing-symbolic")
            })
    }

    /// A file the sandbox cannot decode, or will not read, comes back as
    /// image-missing and never as itself, which GTK would decode in this
    /// process; the second ask is answered at once, without the sandbox.
    #[test]
    fn refused_icon_shows_missing() {
        big_testkit::harness::require_gtk_test(|| {
            let dir = scratch("refused");
            let truncated = dir.join("truncated.png");
            let whole = png();
            std::fs::write(&truncated, &whole[..whole.len() / 2]).unwrap();
            let broken_svg = dir.join("broken.svg");
            std::fs::write(&broken_svg, b"<svg").unwrap();
            for path in [&truncated, &broken_svg] {
                let (first, _) = answer(&file_icon(path));
                assert!(missing(&first), "{}: {first:?}", path.display());
                let (again, at_once) = answer(&file_icon(path));
                assert!(missing(&again) && at_once, "{}: {again:?}", path.display());
            }
            let remote: gio::Icon =
                gio::FileIcon::new(&gio::File::for_uri("https://example.invalid/a.png")).upcast();
            assert!(missing(&answer(&remote).0));
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    /// Themes link many icons to one file: the link is decoded like its file.
    #[test]
    fn linked_icon_decodes() {
        big_testkit::harness::require_gtk_test(|| {
            let dir = scratch("linked");
            let target = dir.join("target.png");
            std::fs::write(&target, png()).unwrap();
            let link = dir.join("link.png");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            let (linked, _) = answer(&file_icon(&link));
            let _ = std::fs::remove_dir_all(&dir);
            assert!(linked.is_ok(), "{linked:?}");
        });
    }

    /// A symbolic theme icon is left to GTK, which recolors it; a file icon
    /// named like one is still read by the sandbox.
    #[test]
    fn symbolic_theme_icon_left_to_gtk() {
        big_testkit::harness::require_gtk_test(|| {
            let themed: gio::Icon = gio::ThemedIcon::new("folder-symbolic").upcast();
            let (answer_themed, at_once) = answer(&themed);
            assert!(at_once);
            assert_eq!(answer_themed.err().as_ref(), Some(&themed));

            let dir = scratch("symbolic");
            let named = dir.join("trap-symbolic.svg");
            std::fs::write(
                &named,
                br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect width="8" height="8"/></svg>"#,
            )
            .unwrap();
            let (from_file, _) = answer(&file_icon(&named));
            let _ = std::fs::remove_dir_all(&dir);
            assert!(from_file.is_ok(), "{from_file:?}");
        });
    }

    /// A recycled image set again before its first icon arrived shows the
    /// second icon, not the late first one.
    #[test]
    fn late_answer_dropped() {
        big_testkit::harness::require_gtk_test(|| {
            let dir = scratch("late");
            let first = dir.join("first.png");
            std::fs::write(&first, png()).unwrap();
            let image = gtk::Image::new();
            image.set_pixel_size(32);
            set_gicon(&image, &file_icon(&first));
            let second: gio::Icon = gio::ThemedIcon::new("folder-symbolic").upcast();
            set_gicon(&image, &second);
            // The first decode ends, or is given up on.
            let first_key = source_key(&first, 32, "");
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline && memory_hit(&first_key).is_none() {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = std::fs::remove_dir_all(&dir);
            assert!(memory_hit(&first_key).is_some(), "the first icon decoded");
            assert_eq!(image.gicon().as_ref(), Some(&second));
        });
    }

    #[test]
    fn old_disk_entries_are_pruned() {
        let dir = std::env::temp_dir().join(format!("big-icon-prune-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        let entry = |name: &str, age: Duration| {
            let path = dir.join(name);
            std::fs::File::create(&path)
                .unwrap()
                .set_modified(now - age)
                .unwrap();
            path
        };
        let day = Duration::from_secs(24 * 60 * 60);
        let old = entry("old.rgba", 31 * day);
        let fresh = entry("fresh.rgba", day);
        prune_disk_cache(&dir, now);
        assert!(!old.exists());
        assert!(fresh.exists());

        let old = entry("old.rgba", 31 * day);
        prune_disk_cache(&dir, now + day / 2);
        assert!(old.exists(), "pruned less than a day ago");
        prune_disk_cache(&dir, now + day);
        assert!(!old.exists());
        assert!(fresh.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An entry read after half its life is marked again; a younger one keeps
    /// its time, so a read costs no write.
    #[test]
    fn read_entry_stays_fresh() {
        let dir = std::env::temp_dir().join(format!("big-icon-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut bytes = [1u8, 0, 0, 0, 1, 0, 0, 0].to_vec();
        bytes.extend([0, 0, 0, 255]);
        let day = Duration::from_secs(24 * 60 * 60);
        let written = |name: &str, age: Duration| {
            let path = dir.join(name);
            std::fs::write(&path, &bytes).unwrap();
            let when = SystemTime::now() - age;
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(when)
                .unwrap();
            (path, when)
        };
        let (old, _) = written("old.rgba", 20 * day);
        let (young, young_time) = written("young.rgba", day);
        let modified = |path: &Path| std::fs::metadata(path).unwrap().modified().unwrap();
        let read = (
            read_disk_entry(&old).is_some(),
            read_disk_entry(&young).is_some(),
        );
        let (old_age, young_after) = (modified(&old).elapsed().unwrap(), modified(&young));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(read, (true, true));
        assert!(old_age < day, "marked again: {old_age:?}");
        assert_eq!(young_after, young_time);
    }

    #[test]
    fn source_key_tracks_size_variant_stamp() {
        let dir = std::env::temp_dir().join(format!("big-icon-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.svg");
        std::fs::write(&path, b"<svg/>").unwrap();
        let a = source_key(&path, 48, "");
        assert_ne!(a, source_key(&path, 64, ""));
        assert_ne!(a, source_key(&path, 48, "#ffffff"));
        std::fs::write(&path, b"<svg></svg>").unwrap();
        assert_ne!(a, source_key(&path, 48, ""), "a changed file is a new key");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
