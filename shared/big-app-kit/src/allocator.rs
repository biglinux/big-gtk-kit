// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The allocator every windowed executable in this suite asks for.
//!
//! One copy of the linkage, the tuning symbol and the reference that keeps the
//! linker from dropping it, so no executable can get the tuning wrong on its own.

/// Link jemalloc into this executable and tune it for a windowed program.
///
/// Expands to nothing unless the calling crate is built with its own
/// `gtk-jemalloc` feature, and must be invoked at the top level of a binary
/// crate: the choice of allocator belongs to whoever owns `main`. A shared
/// object that linked it would put a second allocator in somebody else's
/// process.
///
/// The tuning is a symbol rather than the `MALLOC_CONF` variable, because the
/// variable is inherited across `exec` and would hand this allocator's settings
/// to every program a person launches. `narenas:2` because the default is four
/// times the CPU count, for a program that allocates
/// almost everything on one thread, and every arena it touches keeps dirty
/// pages of its own. `dirty_decay_ms:0` returns a page when it is freed rather
/// than ten seconds later, which for an idle window means never: the decay
/// timer only advances when something calls the allocator.
/// `slab_sizes:1-4096:1` gives every size class up to 4 KiB a one-page slab
/// instead of one sized to fit many objects, so a page empties and goes back
/// as soon as the few objects on it are freed.
///
/// The thread cache stays on: turning it off costs CPU. Measure the programs'
/// memory again before changing any of this.
///
/// Call [`keep_jemalloc_linked`] once at the top of `main`: direct ELF linkage
/// affects this executable only, and an exec'd child keeps its own allocator.
///
/// [`keep_jemalloc_linked`]: macro@crate::link_jemalloc
#[macro_export]
macro_rules! link_jemalloc {
    () => {
        #[cfg(feature = "gtk-jemalloc")]
        #[link(name = "jemalloc")]
        unsafe extern "C" {
            fn mallctl(
                name: *const ::std::ffi::c_char,
                oldp: *mut ::std::ffi::c_void,
                oldlenp: *mut usize,
                newp: *mut ::std::ffi::c_void,
                newlen: usize,
            ) -> ::std::ffi::c_int;
        }

        /// What jemalloc reads before `main`, in this process only.
        #[cfg(feature = "gtk-jemalloc")]
        #[unsafe(no_mangle)]
        // bigdesktop's census measures with the same string.
        pub static malloc_conf: &[u8; 47] = b"narenas:2,dirty_decay_ms:0,slab_sizes:1-4096:1\0";

        /// Reference the library so the linker keeps the dependency, and with
        /// it the tuning symbol above.
        #[cfg(feature = "gtk-jemalloc")]
        fn keep_jemalloc_linked() {
            ::std::hint::black_box(
                mallctl
                    as unsafe extern "C" fn(
                        *const ::std::ffi::c_char,
                        *mut ::std::ffi::c_void,
                        *mut usize,
                        *mut ::std::ffi::c_void,
                        usize,
                    ) -> ::std::ffi::c_int,
            );
        }

        /// Built without the feature: there is no allocator to keep.
        #[cfg(not(feature = "gtk-jemalloc"))]
        fn keep_jemalloc_linked() {}
    };
}
