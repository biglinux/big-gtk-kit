// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Themed and file icons drawn at the size they are shown. GTK lays out an SVG
//! icon's `<text>` with Pango on a worker thread, which is not thread-safe, so
//! every SVG icon is decoded by glycin in its sandbox instead and cached in
//! memory and on disk for the whole suite. Rasters are left to GTK.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

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
/// Byte ceiling for the icon cache, so a handful of large icons can't pin tens
/// Byte ceiling for the icons kept after their widgets are gone.
///
/// It was 32 MiB, and the full-screen menu, which shows every application,
/// filled it: 35 MB more in the desktop after ninety opens, kept for the rest of
/// the session with the menu closed. An icon still on screen is found through
/// `LIVE` whatever this says; this only decides how many recently closed ones
/// come back without reading their few kilobytes from the disk cache.
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

/// What to do with a texture once it exists.
pub type Deliver = Box<dyn FnOnce(gdk::Texture)>;

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
    static WAITERS: RefCell<HashMap<String, Vec<Deliver>>> = RefCell::new(HashMap::new());
    /// Decodes not yet started, in request order.
    static PENDING: RefCell<VecDeque<PendingSvg>> = const { RefCell::new(VecDeque::new()) };
    /// Bytes of SVG source retained by `PENDING`, kept in step with it.
    static PENDING_BYTES: Cell<usize> = const { Cell::new(0) };
    /// Decodes running right now.
    static IN_FLIGHT: Cell<usize> = const { Cell::new(0) };
    /// Where the theme found each icon asked for, by the icon's string form, size
    /// and scale: the file and its cache key, or `None` for "let GTK draw it".
    /// Forgotten whenever the theme changes.
    static RESOLVED: RefCell<HashMap<(String, i32, i32), Resolved>> = RefCell::new(HashMap::new());
    static WATCHING_THEME: Cell<bool> = const { Cell::new(false) };
}

/// What the theme answered for one icon: its file and cache key, or `None` for
/// "let GTK draw it".
type Resolved = Option<(PathBuf, String)>;

/// Ceiling on remembered theme answers; past it the memo starts over.
const MAX_RESOLVED: usize = 4096;

/// The file the icon theme draws `gicon` from at `size`×`scale`, and its cache key.
///
/// Remembered, because a file manager asks the same few dozen file-type icons
/// again for every cell it binds: a folder of 1129 entries asked the theme 1670
/// times and read the icon file's metadata as often, on the main thread, to reach
/// the same twenty answers.
fn resolve(display: &gdk::Display, gicon: &gio::Icon, size: i32, scale: i32) -> Resolved {
    let theme = gtk::IconTheme::for_display(display);
    if !WATCHING_THEME.replace(true) {
        theme.connect_changed(|_| RESOLVED.with(|memo| memo.borrow_mut().clear()));
    }
    // Themed icons only: a file icon's file can change under the same name, and
    // its key has to see the new modification time.
    let name = gicon
        .is::<gio::ThemedIcon>()
        .then(|| IconExt::to_string(gicon))
        .flatten()
        .map(|name| (name.to_string(), size, scale));
    if let Some(name) = &name
        && let Some(answer) = RESOLVED.with(|memo| memo.borrow().get(name).cloned())
    {
        return answer;
    }
    let paintable = theme.lookup_by_gicon(
        gicon,
        size,
        scale,
        gtk::TextDirection::None,
        gtk::IconLookupFlags::FORCE_REGULAR,
    );
    let answer = paintable.file().and_then(|file| file.path()).map(|path| {
        let key = source_key(&path, (size * scale).max(1), "");
        (path, key)
    });
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
/// An SVG behind the icon is drawn by the sandbox at that size, a raster is
/// decoded at that size off the main thread; either arrives a moment later (at
/// once when cached). A symbolic icon and a lookup miss go to GTK: a symbolic
/// icon is recoloured by the theme as it is drawn, which a bitmap cannot be.
pub fn set_gicon(image: &gtk::Image, gicon: &gio::Icon) {
    let size = image.pixel_size().max(16);
    let scale = image.scale_factor().max(1);
    let weak = image.downgrade();
    let fallback = gicon.clone();
    gicon_texture(&image.display(), gicon, size, scale, move |texture| {
        let Some(image) = weak.upgrade() else {
            return;
        };
        match texture {
            Some(texture) => image.set_paintable(Some(&texture)),
            None => image.set_from_gicon(&fallback),
        }
    });
}

/// [`gicon_texture`] as a plain function, for a view that takes an icon loader
/// (`big_item_view::IconLoader`).
pub const GICON_TEXTURE: fn(&gdk::Display, &gio::Icon, i32, i32, DeliverIcon) = gicon_texture;

/// Where [`GICON_TEXTURE`] hands the picture.
pub type DeliverIcon = Box<dyn FnOnce(Option<gdk::Texture>)>;

/// Deliver `gicon` drawn `size`×`scale` pixels, the way [`set_gicon`] draws it, for
/// a surface that takes a texture rather than a `gtk::Image`: a preview tile, a
/// drag icon.
///
/// `None` means "let GTK draw it": a symbolic icon (recoloured by the theme as it
/// is drawn, which a bitmap cannot be), a lookup that found no file, or a raster
/// the sandbox could not decode. An SVG that fails to render delivers nothing.
/// `deliver` runs on the main thread, at once when cached.
///
/// GTK's own loader keeps a raster icon at the size of its file (pixmaps ship
/// 1024-pixel PNGs), and every image embedded in an SVG for the life of the
/// process; the sandbox decodes at the size shown instead.
pub fn gicon_texture(
    display: &gdk::Display,
    gicon: &gio::Icon,
    size: i32,
    scale: i32,
    deliver: impl FnOnce(Option<gdk::Texture>) + 'static,
) {
    // Only the first name: GLib appends a `-symbolic` twin of every name.
    if gicon
        .downcast_ref::<gio::ThemedIcon>()
        .and_then(|themed| themed.names().first().cloned())
        .is_some_and(|name| name.ends_with("-symbolic"))
    {
        deliver(None);
        return;
    }
    let size = size.max(16);
    let scale = scale.max(1);
    let Some((path, key)) = resolve(display, gicon, size, scale) else {
        deliver(None);
        return;
    };
    let px = (size * scale).max(1);
    if path.extension().and_then(|ext| ext.to_str()) != Some("svg") {
        raster_texture(path, (px, px), key, deliver);
        return;
    }
    if let Some(texture) = memory_hit(&key) {
        deliver(Some(texture));
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
            return;
        };
        svg_texture(key, glib::Bytes::from_owned(bytes), px, move |texture| {
            deliver(Some(texture));
        });
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
        let Some((width, height, stride, pixels)) = pixels else {
            deliver(None);
            return;
        };
        let texture = texture_from_rgba(width, height, stride, pixels);
        if fresh {
            disk_put(&key, &texture);
        }
        memory_put(key, texture.clone());
        deliver(Some(texture));
    });
}

/// Decode the raster at `path` to fit inside `bound`: `(width, height, stride,
/// premultiplied RGBA)`. Worker thread only.
///
/// Through this crate's own sandboxed decoder, whatever the format (PNG, JPEG,
/// XPM, ICO, WebP…), and never gdk-pixbuf: its glycin loader copies every file
/// to `/tmp` before the sandbox sees it. `None` when the sandbox cannot decode
/// it, and the caller hands the icon to GTK as the last resort.
fn reduced_raster(path: &Path, bound: (i32, i32)) -> Option<(i32, i32, usize, Vec<u8>)> {
    let image =
        crate::decode_rgba_sandboxed(&crate::BigImageSpec::bounded(path, bound.0, bound.1)).ok()?;
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
/// it is done. `deliver` runs on the main thread either way.
///
/// `key` names the source (see [`source_key`]); two callers asking for the same
/// key while a decode is in flight share it.
pub fn svg_texture(
    key: String,
    bytes: glib::Bytes,
    height_px: i32,
    deliver: impl FnOnce(gdk::Texture) + 'static,
) {
    if let Some(texture) = memory_hit(&key) {
        deliver(texture);
        return;
    }
    if let Some(texture) = disk_hit(&key) {
        memory_put(key, texture.clone());
        deliver(texture);
        return;
    }
    let first_asker = WAITERS.with(|waiters| {
        let mut waiters = waiters.borrow_mut();
        let entry = waiters.entry(key.clone()).or_default();
        entry.push(Box::new(deliver));
        entry.len() == 1
    });
    if first_asker {
        // Bound the speculative queue: under a flood of icon requests, refuse to
        // enqueue past the job or byte ceiling rather than growing PENDING and
        // the SVG bytes it holds without limit. A refused tile keeps its
        // placeholder; a later request retries.
        let over_budget = PENDING.with(|pending| pending.borrow().len()) >= MAX_PENDING_JOBS
            || PENDING_BYTES.with(Cell::get).saturating_add(bytes.len()) > MAX_PENDING_BYTES;
        if over_budget {
            WAITERS.with(|waiters| {
                waiters.borrow_mut().remove(&key);
            });
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
            let waiters = WAITERS
                .with(|waiters| waiters.borrow_mut().remove(&job.key))
                .unwrap_or_default();
            if let Some(texture) = texture {
                memory_put(job.key.clone(), texture.clone());
                disk_put(&job.key, &texture);
                for deliver in waiters {
                    deliver(texture.clone());
                }
            }
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
