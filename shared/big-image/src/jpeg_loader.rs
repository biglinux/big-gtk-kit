// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Route this process's JPEG and PNG decoding through the `glycin-jpegturbo`
//! loader, which decodes both at the size asked for.
//! GLib reads `XDG_DATA_DIRS` once per process, so [`activate`] puts a directory
//! naming our loader in front of it just long enough for GLib to read it, then
//! restores the variable: programs this process starts get the environment it was
//! given. Call it at the top of `main`, before any thread or image load.

use std::path::{Path, PathBuf};

/// Point this process at the reduced-size JPEG and PNG loader: the one beside
/// this executable (a development build), else `installed`, where the program's
/// package puts it.
///
/// A no-op (system loaders unchanged) when neither exists, so a partial build
/// still runs.
///
/// # Safety
///
/// Sets and then restores `XDG_DATA_DIRS`. On Unix both are undefined behavior
/// with a concurrent environment reader, so the caller must run this before any
/// other thread and before anything asks GLib for its data directories, i.e. at
/// the very top of `main`.
pub unsafe fn activate(installed: &Path) {
    if let Some(loader) = find_loader(installed) {
        // SAFETY: the caller upholds the contract above.
        unsafe { activate_with(&loader) }
    }
}

/// [`activate`] with a known loader binary.
///
/// # Safety
///
/// As [`activate`].
pub unsafe fn activate_with(loader: &Path) {
    // glycin then reads that directory alone and ignores the ones routed here,
    // so no type is claimed: the size cap is lifted only for our loader.
    if std::env::var_os("GLYCIN_DATA_DIR").is_some() {
        return;
    }
    // A person's own loader for a type comes before any system directory and
    // wins; the size cap is lifted only for ours, so leave theirs alone.
    let own = gtk::glib::user_data_dir().join(CONF_D);
    let routed: Vec<&Routed> = ROUTED
        .iter()
        .filter(|routed| !claims(&own, routed.mime_type))
        .collect();
    if routed.is_empty() {
        return;
    }
    // One directory per loader binary and set of types, so a development build
    // never rewrites the list an installed desktop is still to read. Its name
    // says which types it routes, for the decoders to read back.
    let types: Vec<&str> = routed.iter().map(|routed| routed.name).collect();
    let base = loaders_base()
        .join(
            loader
                .to_string_lossy()
                .trim_start_matches('/')
                .replace('/', "%"),
        )
        .join(types.join("+"));
    let conf_d = base.join(CONF_D);
    let entries: String = routed
        .iter()
        .map(|routed| {
            format!(
                "[loader:{}]\nExec={}\nIdentifiers={}\nExtension={}\n",
                routed.mime_type,
                loader.display(),
                routed.identifiers,
                routed.name
            )
        })
        .collect();
    // Another instance may be reading it: replace, never truncate.
    let written =
        big_os_kit::storage::atomic_write(&conf_d.join("bigjpeg.conf"), entries.as_bytes());
    if let Err(error) = written {
        gtk::glib::g_warning!("big-image", "cannot write the image loader list: {error}");
        return;
    }
    let given = std::env::var_os("XDG_DATA_DIRS");
    // GLib's own default when the variable is unset.
    let behind = given
        .clone()
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let mut ours = base.into_os_string();
    ours.push(":");
    ours.push(behind);
    // SAFETY: the caller upholds the single-threaded-at-startup contract.
    unsafe {
        std::env::set_var("XDG_DATA_DIRS", &ours);
    }
    let _ = gtk::glib::system_data_dirs();
    // SAFETY: as above.
    unsafe {
        match given {
            Some(dirs) => std::env::set_var("XDG_DATA_DIRS", dirs),
            None => std::env::remove_var("XDG_DATA_DIRS"),
        }
    }
    if routed_types().is_none() {
        gtk::glib::g_warning!(
            "big-image",
            "GLib read its data directories before the image loader was set"
        );
    }
}

/// A type our loader decodes at the size asked for.
struct Routed {
    mime_type: &'static str,
    /// The file name extension, which also names the type in the directory.
    name: &'static str,
    /// The magic bytes glycin matches, in its `conf.d` syntax.
    identifiers: &'static str,
}

const ROUTED: &[Routed] = &[
    Routed {
        mime_type: "image/jpeg",
        name: "jpg",
        identifiers: "0:\\xFF\\xD8\\xFF;",
    },
    // An animated PNG is `image/apng`, which stays with glycin's own loader.
    Routed {
        mime_type: "image/png",
        name: "png",
        identifiers: "0:\\x89PNG;",
    },
];

/// The types this process decodes through our loader, as the name of its
/// directory lists them, or `None` when it decodes none.
///
/// Asked of GLib rather than kept in a static: each module of the host has its
/// own copy of this crate, and only libglib is one per process.
fn routed_types() -> Option<String> {
    let dirs = gtk::glib::system_data_dirs();
    let first = dirs.first()?;
    first.starts_with(loaders_base()).then(|| {
        first
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

/// Whether this process decodes JPEG through our loader, and so at reduced size.
pub(crate) fn large_jpeg_source() -> bool {
    routed_types().is_some_and(|types| types.split('+').any(|name| name == "jpg"))
}

/// Whether this process decodes PNG through our loader, a row at a time at
/// the size asked for.
pub(crate) fn reduced_png_source() -> bool {
    routed_types().is_some_and(|types| types.split('+').any(|name| name == "png"))
}

const CONF_D: &str = "glycin-loaders/2+/conf.d";

/// Where our loader lists live, one data directory per loader binary. In the session's runtime
/// directory, which lasts exactly as long as the programs that read it.
fn loaders_base() -> PathBuf {
    gtk::glib::user_runtime_dir().join("big-image")
}

/// Whether a glycin `conf.d` directory names a loader for `mime_type`.
fn claims(conf_d: &Path, mime_type: &str) -> bool {
    loader_types(conf_d)
        .iter()
        .any(|claimed| claimed == mime_type)
}

/// The types a glycin `conf.d` directory names a loader for, read as glycin
/// reads it: a `.conf` file's group `loader:<type>`, anything after a further
/// `:` ignored, with an `Exec`.
pub(crate) fn loader_types(conf_d: &Path) -> Vec<String> {
    let mut types = Vec::new();
    let lists = std::fs::read_dir(conf_d).into_iter().flatten().flatten();
    for list in lists.filter(|entry| {
        entry
            .path()
            .extension()
            .is_some_and(|suffix| suffix == "conf")
    }) {
        let keys = gtk::glib::KeyFile::new();
        if keys
            .load_from_file(list.path(), gtk::glib::KeyFileFlags::NONE)
            .is_err()
        {
            continue;
        }
        for group in keys.groups() {
            let group = group.as_str().trim();
            let mut parts = group.split(':');
            if parts.next() == Some("loader")
                && let Some(mime_type) = parts.next()
                && keys.string(group, "Exec").is_ok()
            {
                types.push(mime_type.to_owned());
            }
        }
    }
    types
}

/// The loader binary: beside this executable (a dev build), else `installed`.
fn find_loader(installed: &Path) -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let beside = dir.join("glycin-jpegturbo");
        if beside.is_file() {
            return Some(beside);
        }
    }
    installed.is_file().then(|| installed.to_path_buf())
}

#[cfg(test)]
mod tests {
    /// JPEG is routed here, lifting the pixel cap, unless glycin reads only
    /// `GLYCIN_DATA_DIR` or the person's own list names a JPEG loader. Each case
    /// runs in a child, the only thread that touches its environment.
    #[test]
    fn glycin_data_dir_routes_nothing() {
        const CHILD: &str = "BIG_IMAGE_CHILD_ROUTING";
        if let Some(expected) = std::env::var_os(CHILD) {
            let loader = std::env::current_exe().expect("test binary");
            // SAFETY: the child runs this one test before GLib reads its data
            // directories, on the only thread that touches the environment.
            unsafe { super::activate_with(&loader) };
            assert_eq!(super::large_jpeg_source(), expected == "routed");
            return;
        }
        // The routed child writes its loader list under the runtime directory.
        let scratch =
            std::env::temp_dir().join(format!("big-image-routing-{}", std::process::id()));
        let runtime = scratch.join("runtime");
        std::fs::create_dir_all(&runtime).expect("scratch runtime directory");
        let nowhere = scratch.join("glycin-data");
        let empty_home = scratch.join("empty-home");
        let own_home = scratch.join("own-home");
        let own_list = own_home.join(super::CONF_D);
        std::fs::create_dir_all(&own_list).expect("own loader list directory");
        std::fs::write(
            own_list.join("own.conf"),
            "[loader:image/jpeg:local]\nExec=/usr/bin/true\n",
        )
        .expect("own loader list");
        let mut outcomes = Vec::new();
        for (glycin_data_dir, data_home, expected) in [
            (None, &empty_home, "routed"),
            (Some(&nowhere), &empty_home, "unrouted"),
            (None, &own_home, "unrouted"),
        ] {
            let mut child = big_os_kit::subprocess::BigSubprocessSpec::builder()
                .program(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "jpeg_loader::tests::glycin_data_dir_routes_nothing",
                    "--quiet",
                ])
                .env(CHILD, expected)
                .env("XDG_RUNTIME_DIR", &runtime)
                .env("XDG_DATA_HOME", data_home);
            if let Some(directory) = glycin_data_dir {
                child = child.env("GLYCIN_DATA_DIR", directory);
            }
            outcomes.push((expected, child.build().run()));
        }
        std::fs::remove_dir_all(&scratch).expect("scratch removed");
        for (expected, out) in outcomes {
            let out = out.expect("child runs");
            assert!(
                out.status.success() && out.stdout_lossy().contains("1 passed"),
                "{expected}: {}",
                out.stdout_lossy()
            );
        }
    }
}
