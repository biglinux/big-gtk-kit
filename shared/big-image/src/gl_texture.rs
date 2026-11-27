// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Upload a decoded image into a GL texture, so the CPU buffer can be released
//! after upload. Only under the GL renderer: [`rgba_to_gl_texture`] returns `None`
//! without a usable GL context. [`renderer_draws_in_software`] tells llvmpipe
//! behind the GL renderer. The GL entry points come from `libepoxy` through
//! `dlsym`, each a data symbol `epoxy_glX` holding the callable stub.

use std::ffi::{CStr, c_void};
use std::sync::OnceLock;

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::BigRgbaImage;

const GL_RENDERER: u32 = 0x1F01;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_BINDING_2D: u32 = 0x8069;
const GL_MAX_TEXTURE_SIZE: u32 = 0x0D33;
const GL_PIXEL_UNPACK_BUFFER: u32 = 0x88EC;
const GL_PIXEL_UNPACK_BUFFER_BINDING: u32 = 0x88EF;
const GL_UNPACK_ALIGNMENT: u32 = 0x0CF5;
const GL_UNPACK_ROW_LENGTH: u32 = 0x0CF2;
const GL_UNPACK_SKIP_ROWS: u32 = 0x0CF3;
const GL_UNPACK_SKIP_PIXELS: u32 = 0x0CF4;
const GL_NO_ERROR: u32 = 0;
const GL_SYNC_GPU_COMMANDS_COMPLETE: u32 = 0x9117;
const GL_RGBA: u32 = 0x1908;
const GL_RGBA8: i32 = 0x8058;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_LINEAR: i32 = 0x2601;
const GL_CLAMP_TO_EDGE: i32 = 0x812F;

/// The GL entry points needed to name the driver and to create and destroy one
/// 2D texture. Plain C function pointers, so the set is `Send` and can travel
/// into the destroy closure GTK requires to be `Send`.
#[derive(Clone, Copy)]
struct GlFns {
    get_string: unsafe extern "C" fn(u32) -> *const u8,
    get_integerv: unsafe extern "C" fn(u32, *mut i32),
    get_error: unsafe extern "C" fn() -> u32,
    pixel_storei: unsafe extern "C" fn(u32, i32),
    bind_buffer: unsafe extern "C" fn(u32, u32),
    gen_textures: unsafe extern "C" fn(i32, *mut u32),
    bind_texture: unsafe extern "C" fn(u32, u32),
    tex_parameteri: unsafe extern "C" fn(u32, u32, i32),
    tex_image_2d: unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void),
    delete_textures: unsafe extern "C" fn(i32, *const u32),
    fence_sync: unsafe extern "C" fn(u32, u32) -> *mut c_void,
    delete_sync: unsafe extern "C" fn(*mut c_void),
    flush: unsafe extern "C" fn(),
}

/// Read a libepoxy dispatch pointer variable and reinterpret its stored value as
/// the GL function type `T`.
///
/// # Safety
///
/// `handle` must be a live libepoxy handle and `name` an `epoxy_gl*` dispatch
/// pointer whose stored value has the exact signature `T` (same size as a
/// pointer).
unsafe fn dispatch<T: Copy>(handle: *mut c_void, name: &CStr) -> Option<T> {
    // `dlsym` on a data symbol returns the address of the pointer variable; the
    // stored value (the callable dispatch stub) needs one more dereference.
    // SAFETY: the caller passes a live handle; `name` is a NUL-terminated CStr.
    let address = unsafe { libc::dlsym(handle, name.as_ptr()) };
    if address.is_null() {
        return None;
    }
    // SAFETY: a non-null dispatch symbol is a pointer variable holding a `T`,
    // as the caller promises; libepoxy keeps it mapped while the handle lives.
    Some(unsafe { *(address as *const T) })
}

/// Resolve the GL functions from libepoxy exactly once.
fn gl_fns() -> Option<&'static GlFns> {
    static FNS: OnceLock<Option<GlFns>> = OnceLock::new();
    FNS.get_or_init(|| {
        // SAFETY: both names are NUL-terminated literals, and libepoxy is
        // already loaded by GTK, so opening it runs no new initializers.
        let handle = unsafe { libc::dlopen(c"libepoxy.so.0".as_ptr(), libc::RTLD_NOW) };
        let handle = if handle.is_null() {
            // SAFETY: as above.
            unsafe { libc::dlopen(c"libepoxy.so".as_ptr(), libc::RTLD_NOW) }
        } else {
            handle
        };
        if handle.is_null() {
            // No libepoxy: the caller falls back to a CPU memory texture.
            return None;
        }
        // SAFETY: each name is a libepoxy dispatch pointer with the matching
        // signature; the handle stays open for the process's life.
        unsafe {
            Some(GlFns {
                get_string: dispatch(handle, c"epoxy_glGetString")?,
                get_integerv: dispatch(handle, c"epoxy_glGetIntegerv")?,
                get_error: dispatch(handle, c"epoxy_glGetError")?,
                pixel_storei: dispatch(handle, c"epoxy_glPixelStorei")?,
                bind_buffer: dispatch(handle, c"epoxy_glBindBuffer")?,
                gen_textures: dispatch(handle, c"epoxy_glGenTextures")?,
                bind_texture: dispatch(handle, c"epoxy_glBindTexture")?,
                tex_parameteri: dispatch(handle, c"epoxy_glTexParameteri")?,
                tex_image_2d: dispatch(handle, c"epoxy_glTexImage2D")?,
                delete_textures: dispatch(handle, c"epoxy_glDeleteTextures")?,
                fence_sync: dispatch(handle, c"epoxy_glFenceSync")?,
                delete_sync: dispatch(handle, c"epoxy_glDeleteSync")?,
                flush: dispatch(handle, c"epoxy_glFlush")?,
            })
        }
    })
    .as_ref()
}

thread_local! {
    // Weak owners only: no hidden permanent GL context or strong Display cycle.
    // Entries disappear when the last pane/texture stops owning their context.
    static UPLOAD_CONTEXTS: std::cell::RefCell<Vec<(glib::WeakRef<gdk::Display>, glib::WeakRef<gdk::GLContext>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Reuse a live upload context in the default display's share group. The cache
/// is weak and thread-local; returned contexts and textures are the real owners.
/// Uploads must stay on the GTK thread and restore context/unpack state.
///
/// Returns `None` when there is no display or the platform cannot make a GL
/// context; the caller then stays on the CPU texture path.
#[must_use]
pub fn default_gl_context() -> Option<gdk::GLContext> {
    if !gtk::is_initialized_main_thread() {
        return None;
    }
    let display = gdk::Display::default()?;
    if display.is_closed() {
        return None;
    }
    let cached = UPLOAD_CONTEXTS.with_borrow_mut(|entries| {
        entries.retain(|(display, context)| {
            display
                .upgrade()
                .is_some_and(|display| !display.is_closed())
                && context.upgrade().is_some()
        });
        entries.iter().find_map(|(owner, context)| {
            (owner.upgrade().as_ref() == Some(&display))
                .then(|| context.upgrade())
                .flatten()
        })
    });
    if let Some(context) = cached {
        return Some(context);
    }
    // Creating/realizing GObjects is outside the cache borrow, so a signal can
    // safely re-enter unrelated UI work. Preserve the caller's current context.
    let _restore = RestoreCurrentContext(gdk::GLContext::current());
    let context = display.create_gl_context().ok()?;
    context.realize().ok()?;
    UPLOAD_CONTEXTS
        .with_borrow_mut(|entries| entries.push((display.downgrade(), context.downgrade())));
    Some(context)
}

/// Restore the caller's context on every return path, including release callbacks.
struct RestoreCurrentContext(Option<gdk::GLContext>);

impl Drop for RestoreCurrentContext {
    fn drop(&mut self) {
        if let Some(context) = &self.0 {
            context.make_current();
        } else {
            gdk::GLContext::clear_current();
        }
    }
}

fn rgba_len(width: i32, height: i32) -> Option<usize> {
    if width <= 0 || height <= 0 {
        return None;
    }
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)
}

/// State belongs to the caller's currently active texture unit/context, not
/// to our new texture. A bound unpack buffer changes a CPU pointer into a buffer
/// offset; row/skip values can otherwise make GL read beyond our packed payload.
struct RestoreUploadState<'a> {
    gl: &'a GlFns,
    texture: i32,
    buffer: i32,
    unpack: [i32; 4],
}

const UNPACK_NAMES: [u32; 4] = [
    GL_UNPACK_ALIGNMENT,
    GL_UNPACK_ROW_LENGTH,
    GL_UNPACK_SKIP_ROWS,
    GL_UNPACK_SKIP_PIXELS,
];

impl Drop for RestoreUploadState<'_> {
    fn drop(&mut self) {
        // SAFETY: constructed and dropped under the same current GL/ES 3+
        // context. Values are the state queried before any mutation.
        unsafe {
            for (name, value) in UNPACK_NAMES.into_iter().zip(self.unpack) {
                (self.gl.pixel_storei)(name, value);
            }
            (self.gl.bind_buffer)(GL_PIXEL_UNPACK_BUFFER, self.buffer as u32);
            (self.gl.bind_texture)(GL_TEXTURE_2D, self.texture as u32);
        }
    }
}

/// Upload under a current GL/ES 3+ context. The returned name is transferred to
/// GDK by the caller. Do not publish a generated name if allocation/upload failed.
/// Like other GL error checks this consumes an error flag; an error already
/// pending at entry selects fallback, without attempting an upload or draining
/// errors in an unbounded loop. GL error state cannot be restored by the API.
///
/// # Safety
/// A GL/ES 3+ context must be current and `gl` resolved for it.
unsafe fn upload_rgba(gl: &GlFns, width: i32, height: i32, bytes: &[u8]) -> Option<u32> {
    if bytes.len() != rgba_len(width, height)? {
        return None;
    }
    // SAFETY: the caller guarantees the current context and entry point versions.
    unsafe {
        if (gl.get_error)() != GL_NO_ERROR {
            return None;
        }
        let mut maximum = 0;
        (gl.get_integerv)(GL_MAX_TEXTURE_SIZE, &raw mut maximum);
        if (gl.get_error)() != GL_NO_ERROR || maximum <= 0 || width > maximum || height > maximum {
            return None;
        }
        let mut texture = 0;
        let mut buffer = 0;
        let mut unpack = [0; 4];
        (gl.get_integerv)(GL_TEXTURE_BINDING_2D, &raw mut texture);
        (gl.get_integerv)(GL_PIXEL_UNPACK_BUFFER_BINDING, &raw mut buffer);
        for (name, value) in UNPACK_NAMES.into_iter().zip(&mut unpack) {
            (gl.get_integerv)(name, value);
        }
        if (gl.get_error)() != GL_NO_ERROR {
            return None;
        }
        let _restore = RestoreUploadState {
            gl,
            texture,
            buffer,
            unpack,
        };
        let mut id = 0;
        (gl.gen_textures)(1, &raw mut id);
        if id == 0 {
            return None;
        }
        (gl.bind_texture)(GL_TEXTURE_2D, id);
        (gl.bind_buffer)(GL_PIXEL_UNPACK_BUFFER, 0);
        for (name, value) in UNPACK_NAMES.into_iter().zip([1, 0, 0, 0]) {
            (gl.pixel_storei)(name, value);
        }
        (gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        (gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        (gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        (gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
        // Never pass the host pointer if clearing a PBO/unpack state failed.
        if (gl.get_error)() != GL_NO_ERROR {
            (gl.delete_textures)(1, &raw const id);
            return None;
        }
        (gl.tex_image_2d)(
            GL_TEXTURE_2D,
            0,
            GL_RGBA8,
            width,
            height,
            0,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            bytes.as_ptr().cast::<c_void>(),
        );
        if (gl.get_error)() != GL_NO_ERROR {
            (gl.delete_textures)(1, &raw const id);
            return None;
        }
        Some(id)
    }
}

struct GlAllocation {
    id: u32,
    sync: *mut c_void,
}

/// GL/ES fence primitives are core in desktop GL 3.2 and ES 3.0. Older
/// desktop contexts use MemoryTexture instead of calling unavailable stubs.
fn supports_upload_sync(version: (i32, i32), api: gdk::GLAPI) -> bool {
    if api == gdk::GLAPI::GLES {
        version >= (3, 0)
    } else {
        version >= (3, 2)
    }
}

/// The producer submits a fence, and GDK waits on it in another context.
/// A flush submits commands without waiting for the GPU on the GTK thread.
///
/// # Safety
/// The uploader's GL/ES context and the core sync entry points must be usable.
unsafe fn upload_synchronized(gl: &GlFns, image: &BigRgbaImage) -> Option<GlAllocation> {
    // SAFETY: forwarded from the caller, including payload validation.
    let id = unsafe { upload_rgba(gl, image.width, image.height, &image.rgba) }?;
    // SAFETY: the current context supports the core sync API.
    let sync = unsafe { (gl.fence_sync)(GL_SYNC_GPU_COMMANDS_COMPLETE, 0) };
    if sync.is_null() {
        // SAFETY: this texture is unpublished and owned by this context.
        unsafe {
            (gl.delete_textures)(1, &raw const id);
        }
        return None;
    }
    // SAFETY: valid current context; no CPU wait or polling loop.
    unsafe {
        (gl.flush)();
    }
    // SAFETY: valid current context.
    if unsafe { (gl.get_error)() } != GL_NO_ERROR {
        // SAFETY: the fence and texture are unpublished and owned by this context.
        unsafe {
            (gl.delete_sync)(sync);
            (gl.delete_textures)(1, &raw const id);
        }
        return None;
    }
    Some(GlAllocation { id, sync })
}

/// Upload `image` into a GPU-resident [`gdk::GLTexture`], or `None` on any GL
/// failure (the caller falls back to a CPU [`gdk::MemoryTexture`]).
///
/// The returned texture owns the GL texture object and deletes it, with the
/// context current, when the last reference is dropped. `context` must be a
/// realized context from the same display as the renderer.
#[must_use]
pub fn rgba_to_gl_texture(image: &BigRgbaImage, context: &gdk::GLContext) -> Option<gdk::Texture> {
    if image.rgba.len() != rgba_len(image.width, image.height)? {
        return None;
    }
    // All unpack state below is core in both desktop GL 3 and OpenGL ES 3.
    // An older context keeps the CPU fallback rather than probing unsupported
    // entry points or relying on extensions with a different contract.
    if !supports_upload_sync(context.version(), context.api()) {
        return None;
    }
    let gl = gl_fns()?;

    let _restore = RestoreCurrentContext(gdk::GLContext::current());
    context.make_current();
    if gdk::GLContext::current().as_ref() != Some(context) {
        return None;
    }
    // SAFETY: a GL/ES 3+ context is current and the complete RGBA payload was
    // checked. The uploader restores its bindings and unpack state on failure too.
    let allocation = unsafe { upload_synchronized(gl, image) }?;

    // Match the CPU path: an opaque image is X8 so the compositor never blends
    // the dead alpha and the texture can be offloaded to a subsurface.
    let format = if image.opaque {
        gdk::MemoryFormat::R8g8b8x8
    } else {
        gdk::MemoryFormat::R8g8b8a8
    };
    let mut builder = gdk::GLTextureBuilder::new()
        .set_context(Some(context))
        .set_id(allocation.id)
        .set_sync(Some(allocation.sync.cast_const()))
        .set_width(image.width)
        .set_height(image.height)
        .set_format(format);
    if let Some(color_state) = image.cicp.and_then(crate::color_state_for) {
        builder = builder.set_color_state(&color_state);
    }

    // GdkTexture is thread-safe: the last unref, and therefore the destroy
    // callback, may run outside GTK's thread. Move the guard back before use
    // or drop; MainContext::invoke is unsuitable because it can run inline.
    let guarded_context = glib::thread_guard::ThreadGuard::new((context.clone(), allocation));
    let gl = *gl;
    let release = move || {
        let on_owner = guarded_context.is_owner();
        let cleanup = move || {
            let _restore = RestoreCurrentContext(gdk::GLContext::current());
            let (context, allocation) = guarded_context.get_ref();
            context.make_current();
            if gdk::GLContext::current().as_ref() != Some(context) {
                // Never delete the same numeric name in a different context.
                // Context-loss cleanup must be handled by the driver/context.
                return;
            }
            // SAFETY: the texture belongs to this current context and nothing uses it any more.
            unsafe {
                (gl.delete_sync)(allocation.sync);
                (gl.delete_textures)(1, &raw const allocation.id);
            };
        };
        if on_owner {
            cleanup();
        } else {
            // The application must keep the GTK main context alive while its
            // workers release textures, and drain it before final shutdown.
            glib::idle_add_once(cleanup);
        }
    };
    // SAFETY: `id` names the texture just created in `context`; the release
    // closure deletes exactly that texture with the context current.
    let built = unsafe { builder.build_with_release_func(release) };
    Some(built.upcast())
}

/// Build the display texture for `image`, GPU-only under the GL renderer.
///
/// Under the GL renderer the pixels go to a GdkGLTexture (the CPU buffer is not
/// retained by the texture); elsewhere, or on any GL failure, a shared memory
/// texture. `cache` holds the widget's reused GL context: pass the same cell
/// each call so the context is made once. Convenience for callers that own a
/// widget and a decoded frame (previews, thumbnails); the wallpaper and viewer
/// pick their path themselves.
#[must_use]
pub fn rgba_texture_for_widget(
    image: &BigRgbaImage,
    widget: &impl IsA<gtk::Widget>,
    cache: &std::cell::RefCell<Option<gdk::GLContext>>,
) -> Option<gdk::Texture> {
    if renderer_is_gl(widget) {
        if cache.borrow().is_none() {
            *cache.borrow_mut() = default_gl_context();
        }
        if let Some(context) = cache.borrow().as_ref()
            && let Some(texture) = rgba_to_gl_texture(image, context)
        {
            return Some(texture);
        }
    }
    crate::rgba_to_texture(image)
}

/// Whether the display's renderer is the GL one, the only renderer where our
/// own GL texture helps.
///
/// Under the GL renderer a GPU-only texture saves memory against a memory
/// texture (the CPU buffer is freed and the renderer's own GL stack uploads it
/// once). Under the Vulkan renderer it costs more, because making a GL context
/// drags the whole GL driver in alongside Vulkan, so Vulkan keeps the memory
/// texture. Cairo (software) would have to download the texture, so it does too.
#[must_use]
pub fn renderer_is_gl(widget: &impl IsA<gtk::Widget>) -> bool {
    widget
        .native()
        .and_then(|native| native.renderer())
        .is_some_and(|renderer| renderer.type_().name().contains("GLRenderer"))
}

/// Whether `renderer` draws on the processor: Cairo, or GL on llvmpipe. GTK
/// passes over llvmpipe when it picks a renderer itself, but takes it when
/// `GSK_RENDERER=gl` names GL, as a session may on a GPU without a Vulkan
/// driver, and when GL is its fallback after Vulkan.
#[must_use]
pub fn renderer_draws_in_software(renderer: &gtk::gsk::Renderer) -> bool {
    if renderer.is::<gtk::gsk::CairoRenderer>() {
        return true;
    }
    if !renderer.type_().name().contains("GLRenderer") {
        return false;
    }
    thread_local! {
        // The process keeps one GL driver, so it is asked once.
        static LLVMPIPE: std::cell::OnceCell<bool> = const { std::cell::OnceCell::new() };
    }
    LLVMPIPE.with(|llvmpipe| *llvmpipe.get_or_init(gl_is_llvmpipe))
}

/// Whether the display's GL driver is llvmpipe, as GTK tells it: by the name in
/// `GL_RENDERER`. Without libepoxy or a GL context the GL renderer could not
/// draw either, so the answer is no.
fn gl_is_llvmpipe() -> bool {
    let (Some(gl), Some(context)) = (gl_fns(), default_gl_context()) else {
        return false;
    };
    let _restore = RestoreCurrentContext(gdk::GLContext::current());
    context.make_current();
    // SAFETY: a context is current on this thread, and glGetString answers a
    // static NUL-terminated string or null.
    let name = unsafe { (gl.get_string)(GL_RENDERER) };
    if name.is_null() {
        return false;
    }
    // SAFETY: non-null, NUL-terminated and owned by the driver for the
    // context's life, which outlasts this read.
    let name = unsafe { CStr::from_ptr(name.cast()) };
    name.to_bytes().windows(8).any(|word| word == b"llvmpipe")
}

#[cfg(test)]
mod upload_tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Clone)]
    struct State {
        texture: i32,
        buffer: i32,
        unpack: [i32; 4],
        maximum: i32,
        error: u32,
        fail_upload: bool,
        uploads: usize,
        deleted: Vec<u32>,
        packed_upload: bool,
        fail_fence: bool,
        fences: usize,
        flushes: usize,
        sync_deletes: usize,
    }
    impl Default for State {
        fn default() -> Self {
            Self {
                texture: 7,
                buffer: 9,
                unpack: [8, 23, 2, 3],
                maximum: 4096,
                error: 0,
                fail_upload: false,
                uploads: 0,
                deleted: Vec::new(),
                packed_upload: false,
                fail_fence: false,
                fences: 0,
                flushes: 0,
                sync_deletes: 0,
            }
        }
    }
    thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }
    unsafe extern "C" fn get(name: u32, out: *mut i32) {
        let value = STATE.with_borrow(|s| match name {
            GL_TEXTURE_BINDING_2D => s.texture,
            GL_PIXEL_UNPACK_BUFFER_BINDING => s.buffer,
            GL_MAX_TEXTURE_SIZE => s.maximum,
            _ => UNPACK_NAMES
                .iter()
                .position(|&n| n == name)
                .map_or(0, |i| s.unpack[i]),
        });
        // SAFETY: the uploader passes a pointer to a live `i32`.
        unsafe {
            *out = value;
        }
    }
    unsafe extern "C" fn error() -> u32 {
        STATE.with_borrow_mut(|s| std::mem::take(&mut s.error))
    }
    unsafe extern "C" fn pixel(name: u32, value: i32) {
        STATE.with_borrow_mut(|s| {
            if let Some(i) = UNPACK_NAMES.iter().position(|&n| n == name) {
                s.unpack[i] = value;
            }
        });
    }
    unsafe extern "C" fn bind_buffer(_: u32, value: u32) {
        STATE.with_borrow_mut(|s| s.buffer = value as i32);
    }
    unsafe extern "C" fn bind_texture(_: u32, value: u32) {
        STATE.with_borrow_mut(|s| s.texture = value as i32);
    }
    unsafe extern "C" fn generate(_: i32, out: *mut u32) {
        // SAFETY: the uploader passes a pointer to one live `u32`.
        unsafe {
            *out = 42;
        }
    }
    unsafe extern "C" fn parameter(_: u32, _: u32, _: i32) {}
    unsafe extern "C" fn upload(
        _: u32,
        _: i32,
        _: i32,
        _: i32,
        _: i32,
        _: i32,
        _: u32,
        _: u32,
        _: *const c_void,
    ) {
        STATE.with_borrow_mut(|s| {
            s.uploads += 1;
            s.packed_upload = s.buffer == 0 && s.unpack == [1, 0, 0, 0];
            if s.fail_upload {
                s.error = 0x0505;
            }
        });
    }
    unsafe extern "C" fn delete(_: i32, name: *const u32) {
        // SAFETY: the uploader passes a pointer to the one name it generated.
        STATE.with_borrow_mut(|s| s.deleted.push(unsafe { *name }));
    }
    unsafe extern "C" fn fence(condition: u32, flags: u32) -> *mut c_void {
        assert_eq!((condition, flags), (GL_SYNC_GPU_COMMANDS_COMPLETE, 0));
        STATE.with_borrow_mut(|s| {
            s.fences += 1;
            if s.fail_fence {
                std::ptr::null_mut()
            } else {
                std::ptr::dangling_mut::<c_void>()
            }
        })
    }
    unsafe extern "C" fn delete_sync(_: *mut c_void) {
        STATE.with_borrow_mut(|s| s.sync_deletes += 1);
    }
    unsafe extern "C" fn flush() {
        STATE.with_borrow_mut(|s| s.flushes += 1);
    }
    unsafe extern "C" fn string(_: u32) -> *const u8 {
        std::ptr::null()
    }
    fn fns() -> GlFns {
        GlFns {
            get_string: string,
            get_integerv: get,
            get_error: error,
            pixel_storei: pixel,
            bind_buffer,
            gen_textures: generate,
            bind_texture,
            tex_parameteri: parameter,
            tex_image_2d: upload,
            delete_textures: delete,
            fence_sync: fence,
            delete_sync,
            flush,
        }
    }
    #[test]
    fn upload_restores_caller_gl_state() {
        for fail in [false, true] {
            STATE.with_borrow_mut(|s| {
                *s = State {
                    fail_upload: fail,
                    ..State::default()
                }
            });
            assert_eq!(
                // SAFETY: fake entry points only access the test's thread-local state.
                unsafe { upload_rgba(&fns(), 1, 1, &[0; 4]) },
                if fail { None } else { Some(42) }
            );
            STATE.with_borrow(|s| {
                assert_eq!((s.texture, s.buffer, s.unpack), (7, 9, [8, 23, 2, 3]));
                assert!(s.packed_upload);
                assert_eq!(s.deleted.as_slice(), if fail { &[42][..] } else { &[][..] });
            });
        }
    }
    #[test]
    fn invalid_payload_or_oversize_never_uploads() {
        STATE.with_borrow_mut(|s| {
            *s = State {
                maximum: 1,
                ..State::default()
            }
        });
        assert_eq!(rgba_len(0, 2), None);
        assert_eq!(rgba_len(-1, 2), None);
        assert_eq!(rgba_len(2, 3), Some(24));
        // SAFETY: fake entry points as above.
        unsafe {
            assert_eq!(upload_rgba(&fns(), 2, 1, &[0; 8]), None);
            assert_eq!(upload_rgba(&fns(), 1, 1, &[0; 3]), None);
        }
        STATE.with_borrow(|s| assert_eq!(s.uploads, 0));
    }
    #[test]
    fn preexisting_gl_error_publishes_nothing() {
        STATE.with_borrow_mut(|s| {
            *s = State {
                error: 0x0502,
                ..State::default()
            }
        });
        // SAFETY: fake entry points as above.
        assert_eq!(unsafe { upload_rgba(&fns(), 1, 1, &[0; 4]) }, None);
        STATE.with_borrow(|s| assert_eq!(s.uploads, 0));
    }
    #[test]
    fn sync_requires_core_version_floor() {
        assert!(!supports_upload_sync((3, 0), gdk::GLAPI::GL));
        assert!(!supports_upload_sync((3, 1), gdk::GLAPI::GL));
        assert!(supports_upload_sync((3, 2), gdk::GLAPI::GL));
        assert!(supports_upload_sync((4, 6), gdk::GLAPI::GL));
        assert!(!supports_upload_sync((2, 0), gdk::GLAPI::GLES));
        assert!(supports_upload_sync((3, 0), gdk::GLAPI::GLES));
    }

    #[test]
    fn fence_failure_destroys_texture() {
        STATE.with_borrow_mut(|s| {
            *s = State {
                fail_fence: true,
                ..State::default()
            }
        });
        let image = BigRgbaImage {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
            cicp: None,
            opaque: false,
        };
        // SAFETY: `fns` is the fake above, which needs no GL context.
        assert!(unsafe { upload_synchronized(&fns(), &image) }.is_none());
        STATE.with_borrow(|s| {
            assert_eq!(s.deleted, [42]);
            assert_eq!((s.fences, s.flushes, s.sync_deletes), (1, 0, 0));
        });
    }

    #[test]
    fn published_texture_has_fence() {
        STATE.with_borrow_mut(|s| *s = State::default());
        let image = BigRgbaImage {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
            cicp: None,
            opaque: false,
        };
        // SAFETY: `fns` is the fake above, which needs no GL context.
        let allocation = unsafe { upload_synchronized(&fns(), &image) }.unwrap();
        assert_eq!(allocation.id, 42);
        assert!(!allocation.sync.is_null());
        STATE.with_borrow(|s| {
            assert!(s.deleted.is_empty());
            assert_eq!((s.fences, s.flushes, s.sync_deletes), (1, 1, 0));
        });
    }

    #[test]
    #[ignore = "display: needs a GTK display with GL"]
    fn shared_upload_context_is_weak() {
        assert!(
            big_testkit::harness::run_gtk_test(|| {
                let current = gdk::GLContext::current();
                let a = default_gl_context().expect("GL context available");
                let b = default_gl_context().expect("second consumer shares context");
                assert_eq!(a, b);
                assert_eq!(gdk::GLContext::current(), current);
                let weak = a.downgrade();
                let image = BigRgbaImage {
                    width: 1,
                    height: 1,
                    rgba: vec![255, 0, 0, 255],
                    cicp: None,
                    opaque: true,
                };
                let texture = rgba_to_gl_texture(&image, &a).expect("GL upload and fence work");
                assert_eq!(gdk::GLContext::current(), current);
                let mut bytes = [0u8; 4];
                texture.download(&mut bytes, 4);
                // GdkTexture::download is native-endian Cairo ARGB32 (BGRA on AMD64).
                assert_eq!(bytes, [0, 0, 255, 255]);
                // GDK readback may leave the upload context current. This reference
                // is not the weak cache's owner; release it explicitly for the test.
                if let Some(current) = current {
                    current.make_current();
                } else {
                    gdk::GLContext::clear_current();
                }
                drop(a);
                drop(b);
                assert!(
                    weak.upgrade().is_some(),
                    "texture still owns its upload context"
                );
                drop(texture);
                assert!(
                    weak.upgrade().is_none(),
                    "weak pool must not retain the context"
                );
            }),
            "GTK test body must run"
        );
    }
}
