// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Asking the desktop for a file, whichever desktop this is.
//!
//! The XDG portal's `FileChooser` is asked first, so GNOME, KDE and BigLinux
//! each show their own picker; GTK's `GtkFileDialog` answers when no portal
//! does. GTK reaches the portal by itself only for sandboxed applications.
//!
//! The answer is the chosen [`gio::File`]s, local or not. Cancelling is
//! "no file", never an error; an error is a dialog that failed.

use std::path::PathBuf;

use gtk::prelude::*;
use gtk::{gio, glib};

/// What the caller is asking for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRequestKind {
    /// One existing file.
    OpenFile,
    /// Any number of existing files.
    OpenFiles,
    /// A name to write to, pre-filled.
    SaveFile {
        /// The name the entry starts with.
        current_name: String,
    },
    /// One folder.
    SelectFolder,
}

/// One named file type: what the menu says, and what it accepts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileType {
    /// What the person reads.
    pub label: String,
    /// Shell globs, as in `*.png`.
    pub patterns: Vec<String>,
    /// Content types, as in `text/plain`.
    ///
    /// These match by inheritance, so `text/plain` also accepts `text/x-csrc`
    /// and `text/markdown`. That is the difference from a glob and the reason
    /// to prefer one: a shell script with no extension is still text, and no
    /// list of extensions ever ends.
    pub mime_types: Vec<String>,
}

/// A request for a file, to be answered by whatever dialog this desktop uses.
#[derive(Debug, Clone)]
pub struct FileRequest {
    /// What is being asked for.
    pub kind: FileRequestKind,
    /// Window title. Empty asks the desktop for its own wording.
    pub title: String,
    /// Where to start. `None` starts wherever the desktop last was.
    pub start_in: Option<PathBuf>,
    /// A file to start on: selected in the toolkit's dialog, its folder in
    /// the portal's.
    pub select: Option<PathBuf>,
    /// The word on the confirming button. `None` asks for the desktop's.
    pub accept_label: Option<String>,
    /// The file types offered. Empty offers no menu.
    pub types: Vec<FileType>,
}

impl FileRequest {
    /// Ask for an existing file.
    #[must_use]
    pub fn open_file() -> Self {
        Self::of(FileRequestKind::OpenFile)
    }

    /// Ask for any number of existing files.
    #[must_use]
    pub fn open_files() -> Self {
        Self::of(FileRequestKind::OpenFiles)
    }

    /// Ask for somewhere to save, with `current_name` pre-filled.
    #[must_use]
    pub fn save_file(current_name: impl Into<String>) -> Self {
        Self::of(FileRequestKind::SaveFile {
            current_name: current_name.into(),
        })
    }

    /// Ask for a folder.
    #[must_use]
    pub fn select_folder() -> Self {
        Self::of(FileRequestKind::SelectFolder)
    }

    fn of(kind: FileRequestKind) -> Self {
        Self {
            kind,
            title: String::new(),
            start_in: None,
            select: None,
            accept_label: None,
            types: Vec::new(),
        }
    }

    /// Override the window title.
    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Start the browser in `directory`.
    #[must_use]
    pub fn starting_in(mut self, directory: impl Into<PathBuf>) -> Self {
        self.start_in = Some(directory.into());
        self
    }

    /// Start on `file`, selected where the dialog can select it.
    #[must_use]
    pub fn selecting(mut self, file: impl Into<PathBuf>) -> Self {
        self.select = Some(file.into());
        self
    }

    /// Put `label` on the confirming button.
    #[must_use]
    pub fn accepting(mut self, label: impl Into<String>) -> Self {
        self.accept_label = Some(label.into());
        self
    }

    /// Offer a named file type by content type, as in
    /// `("Text Files", ["text/plain"])`.
    #[must_use]
    pub fn offering_content(mut self, label: impl Into<String>, mime_types: &[&str]) -> Self {
        self.types.push(FileType {
            label: label.into(),
            mime_types: mime_types.iter().map(|mime| (*mime).to_owned()).collect(),
            ..FileType::default()
        });
        self
    }

    /// Offer a named file type by its file name suffixes, in any letter case,
    /// as `gtk::FileFilter::add_suffix` matches them: `("Audio", ["mp3"])`
    /// also offers `SONG.MP3`.
    #[must_use]
    pub fn offering_suffixes(self, label: impl Into<String>, suffixes: &[&str]) -> Self {
        let patterns: Vec<String> = suffixes.iter().map(|suffix| suffix_glob(suffix)).collect();
        let patterns: Vec<&str> = patterns.iter().map(String::as_str).collect();
        self.offering(label, &patterns)
    }

    /// Offer a named file type, as in `("Images", ["*.png", "*.jpg"])`.
    #[must_use]
    pub fn offering(mut self, label: impl Into<String>, patterns: &[&str]) -> Self {
        self.types.push(FileType {
            label: label.into(),
            patterns: patterns
                .iter()
                .map(|pattern| (*pattern).to_owned())
                .collect(),
            mime_types: Vec::new(),
        });
        self
    }
}

/// The glob `*.suffix` in any letter case, written as GTK writes a suffix
/// filter for the portal: `mp3` gives `*.[mM][pP]3`.
fn suffix_glob(suffix: &str) -> String {
    let mut glob = String::from("*.");
    for character in suffix.chars() {
        let lower = character.to_lowercase().collect::<String>();
        let upper = character.to_uppercase().collect::<String>();
        if lower == upper {
            if matches!(character, '*' | '?' | '[' | ']') {
                glob.push('[');
                glob.push(character);
                glob.push(']');
            } else {
                glob.push(character);
            }
        } else {
            glob.push('[');
            glob.push_str(&lower);
            glob.push_str(&upper);
            glob.push(']');
        }
    }
    glob
}

/// Ask, and call `chosen` with the answer. `Ok(None)` is a cancel.
pub fn present(
    parent: Option<&gtk::Window>,
    request: FileRequest,
    chosen: impl FnOnce(Result<Option<gio::File>, glib::Error>) + 'static,
) {
    present_multiple(parent, request, move |picked| {
        chosen(picked.map(|files| files.into_iter().next()));
    });
}

/// Ask for several, and call `chosen` with all of them. `Ok` and empty is a
/// cancel.
///
/// The portal first, so the person gets the picker their desktop chose; the
/// toolkit's own dialog when no portal answers, which is what happens on a
/// session that has none.
pub fn present_multiple(
    parent: Option<&gtk::Window>,
    request: FileRequest,
    chosen: impl FnOnce(Result<Vec<gio::File>, glib::Error>) + 'static,
) {
    let parent = parent.cloned();
    // Called once, by whichever dialog answers.
    let chosen = std::rc::Rc::new(std::cell::Cell::new(Some(chosen)));
    // The handle first: a portal dialog with no parent is a window of its own,
    // free to appear behind the one that asked for it.
    let asking = parent.clone();
    exported_parent(asking.as_ref(), move |handle, exported| {
        let fallback = request.clone();
        portal::ask(
            portal_method(&request.kind),
            &handle,
            &request.title,
            portal_options(&request),
            move |answer| {
                // The name is given back once, when the dialog is over and
                // nothing can still be looking it up.
                if let Some((toplevel, handle)) = &exported {
                    toplevel.drop_exported_handle(handle);
                }
                let chosen = std::rc::Rc::clone(&chosen);
                let answer_once = move |picked| {
                    if let Some(chosen) = chosen.take() {
                        chosen(picked);
                    }
                };
                match answer {
                    Some(picked) => answer_once(Ok(picked)),
                    None => toolkit_dialog(parent.as_ref(), fallback.clone(), answer_once),
                }
            },
        );
    });
}

/// [`present`] as a future, for a caller inside an async block.
///
/// # Errors
///
/// The dialog's error, as [`present`] passes it on; a cancel is `Ok(None)`.
pub async fn present_future(
    parent: Option<&gtk::Window>,
    request: FileRequest,
) -> Result<Option<gio::File>, glib::Error> {
    present_multiple_future(parent, request)
        .await
        .map(|files| files.into_iter().next())
}

/// [`present_multiple`] as a future, for a caller inside an async block.
///
/// # Errors
///
/// The dialog's error, as [`present_multiple`] passes it on; a cancel is
/// `Ok` and empty.
pub async fn present_multiple_future(
    parent: Option<&gtk::Window>,
    request: FileRequest,
) -> Result<Vec<gio::File>, glib::Error> {
    let (answer, answered) = futures_channel::oneshot::channel();
    present_multiple(parent, request, move |picked| {
        let _ = answer.send(picked);
    });
    // Every path above answers once; a dialog that went without answering
    // chose nothing.
    answered.await.unwrap_or_else(|_| Ok(Vec::new()))
}

/// The local file of a [`present`] answer, for a caller that takes nothing
/// else: a failed dialog and a file with no local path are logged and read as
/// no file.
#[must_use]
pub fn local_path(answer: Result<Option<gio::File>, glib::Error>) -> Option<PathBuf> {
    local_paths(answer.map(|file| file.into_iter().collect()))
        .into_iter()
        .next()
}

/// The local files of a [`present_multiple`] answer, as [`local_path`] reads
/// one.
#[must_use]
pub fn local_paths(answer: Result<Vec<gio::File>, glib::Error>) -> Vec<PathBuf> {
    match answer {
        Ok(files) => files
            .into_iter()
            .filter_map(|file| {
                let path = file.path();
                if path.is_none() {
                    // The scheme alone: a remote URI may carry a user name.
                    log::warn!(
                        "the file dialog chose a {} file, which is not on this computer",
                        file.uri_scheme().as_deref().unwrap_or("schemeless")
                    );
                }
                path
            })
            .collect(),
        Err(error) => {
            log::warn!("the file dialog ended with {error}");
            Vec::new()
        }
    }
}

/// Name `parent` in the form the portal understands, then carry on.
///
/// An empty name is a legitimate answer (there may be no parent, or this may
/// not be a Wayland session), and the portal treats it as "no parent" rather
/// than an error, so every path here continues rather than giving up.
fn exported_parent(
    parent: Option<&gtk::Window>,
    then: impl FnOnce(String, Option<(gdk4_wayland::WaylandToplevel, String)>) + 'static,
) {
    // Shared, because exactly one of two places must run it: the compositor's
    // reply, or this function when there is not going to be one. A dialog that
    // never opens because both declined is the failure to avoid.
    let then = std::rc::Rc::new(std::cell::RefCell::new(Some(then)));
    let carry_on = {
        let then = std::rc::Rc::clone(&then);
        move |handle: String, exported: Option<(gdk4_wayland::WaylandToplevel, String)>| {
            if let Some(then) = then.borrow_mut().take() {
                then(handle, exported);
            }
        }
    };
    let Some(toplevel) = parent
        .and_then(|window| window.surface())
        .and_downcast::<gdk4_wayland::WaylandToplevel>()
    else {
        carry_on(String::new(), None);
        return;
    };
    let asked = toplevel.export_handle({
        let carry_on = carry_on.clone();
        move |toplevel, handle| match handle {
            // The exported toplevel travels with the name. Giving it back here
            // would invalidate the name before the portal has been told it, and
            // the dialog would open parented to nothing.
            Ok(handle) => carry_on(
                format!("wayland:{handle}"),
                Some((toplevel.clone(), handle.to_owned())),
            ),
            Err(_) => carry_on(String::new(), None),
        }
    });
    if !asked {
        log::debug!("the compositor would not export a window handle");
        carry_on(String::new(), None);
    }
}

/// Which portal method answers this question.
const fn portal_method(kind: &FileRequestKind) -> &'static str {
    match kind {
        FileRequestKind::SaveFile { .. } => "SaveFile",
        _ => "OpenFile",
    }
}

/// The request as the portal's option dictionary.
fn portal_options(request: &FileRequest) -> glib::Variant {
    let options = glib::VariantDict::new(None);
    options.insert("modal", true);
    if let Some(label) = &request.accept_label {
        options.insert("accept_label", label.as_str());
    }
    match &request.kind {
        FileRequestKind::OpenFiles => options.insert("multiple", true),
        // A folder is asked for through OpenFile with this flag, not a method
        // of its own.
        FileRequestKind::SelectFolder => options.insert("directory", true),
        FileRequestKind::SaveFile { current_name } if !current_name.is_empty() => {
            options.insert("current_name", current_name.as_str());
        }
        _ => {}
    }
    let start = request
        .start_in
        .as_deref()
        .or_else(|| request.select.as_deref().and_then(std::path::Path::parent))
        // A bare file name's parent is empty, which names no folder.
        .filter(|start| !start.as_os_str().is_empty());
    if let Some(start) = start {
        // Paths travel as NUL-terminated bytes, not as text: a file name is
        // bytes on Linux and need not be UTF-8.
        let mut bytes = start.as_os_str().as_encoded_bytes().to_vec();
        bytes.push(0);
        options.insert_value(
            "current_folder",
            &glib::Variant::array_from_fixed_array(&bytes),
        );
    }
    if let Some((first, _)) = request.types.split_first() {
        options.insert_value("filters", &portal_filters(&request.types));
        // Without this the dialog starts on whatever it likes: KDE's starts
        // on a combined filter that matches everything, which is the same as
        // having offered nothing.
        options.insert_value("current_filter", &portal_filter(first));
    }
    options.end()
}

/// The offered types as `a(sa(us))`, where 0 marks a shell glob and 1 a
/// content type.
fn portal_filters(types: &[FileType]) -> glib::Variant {
    let filter_type = <(String, Vec<(u32, String)>)>::static_variant_type();
    let filters = types.iter().map(portal_filter).collect::<Vec<_>>();
    glib::Variant::array_from_iter_with_type(&filter_type, &filters)
}

/// One offered type as `(sa(us))`.
fn portal_filter(offered: &FileType) -> glib::Variant {
    let rule_type = <(u32, String)>::static_variant_type();
    let rules = offered
        .patterns
        .iter()
        .map(|pattern| glib::Variant::tuple_from_iter([0u32.to_variant(), pattern.to_variant()]))
        .chain(
            offered
                .mime_types
                .iter()
                .map(|mime| glib::Variant::tuple_from_iter([1u32.to_variant(), mime.to_variant()])),
        )
        .collect::<Vec<_>>();
    glib::Variant::tuple_from_iter([
        offered.label.to_variant(),
        glib::Variant::array_from_iter_with_type(&rule_type, &rules),
    ])
}

/// The toolkit's own dialog, for a desktop with no portal.
fn toolkit_dialog(
    parent: Option<&gtk::Window>,
    request: FileRequest,
    chosen: impl FnOnce(Result<Vec<gio::File>, glib::Error>) + 'static,
) {
    if request.kind == FileRequestKind::OpenFiles {
        build(&request).open_multiple(parent, gio::Cancellable::NONE, move |answer| {
            chosen(match answer {
                Ok(files) => Ok(files.iter::<gio::File>().flatten().collect()),
                Err(error) if dismissed(&error) => Ok(Vec::new()),
                Err(error) => Err(error),
            });
        });
        return;
    }
    let dialog = build(&request);
    match request.kind {
        FileRequestKind::SaveFile { .. } => {
            dialog.save(parent, gio::Cancellable::NONE, move |answer| {
                chosen(one(answer));
            });
        }
        FileRequestKind::SelectFolder => {
            dialog.select_folder(parent, gio::Cancellable::NONE, move |answer| {
                chosen(one(answer));
            });
        }
        _ => dialog.open(parent, gio::Cancellable::NONE, move |answer| {
            chosen(one(answer));
        }),
    }
}

fn build(request: &FileRequest) -> gtk::FileDialog {
    #[expect(clippy::disallowed_methods, reason = "the owner's fallback dialog")]
    let dialog = gtk::FileDialog::builder().modal(true).build();
    if !request.title.is_empty() {
        dialog.set_title(&request.title);
    }
    if let Some(label) = &request.accept_label {
        dialog.set_accept_label(Some(label));
    }
    if let Some(start) = &request.start_in {
        dialog.set_initial_folder(Some(&gio::File::for_path(start)));
    }
    if let Some(file) = &request.select {
        dialog.set_initial_file(Some(&gio::File::for_path(file)));
    }
    if let FileRequestKind::SaveFile { current_name } = &request.kind
        && !current_name.is_empty()
    {
        dialog.set_initial_name(Some(current_name));
    }
    if !request.types.is_empty() {
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        for offered in &request.types {
            let filter = gtk::FileFilter::new();
            filter.set_name(Some(&offered.label));
            for pattern in &offered.patterns {
                filter.add_pattern(pattern);
            }
            for mime in &offered.mime_types {
                filter.add_mime_type(mime);
            }
            filters.append(&filter);
        }
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(filters.item(0).and_downcast::<gtk::FileFilter>().as_ref());
    }
    dialog
}

/// One answer, where dismissing the dialog is "no file" rather than an error.
fn one(answer: Result<gio::File, glib::Error>) -> Result<Vec<gio::File>, glib::Error> {
    match answer {
        Ok(file) => Ok(vec![file]),
        Err(error) if dismissed(&error) => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// The person closed the dialog, or the program took it down: no file, and
/// nothing to report.
fn dismissed(error: &glib::Error) -> bool {
    error.matches(gtk::DialogError::Dismissed) || error.matches(gtk::DialogError::Cancelled)
}

/// Speaking `org.freedesktop.portal.FileChooser` directly.
///
/// GTK only reaches the portal for sandboxed applications, so an unsandboxed
/// program that wants the desktop's chosen picker asks for it itself: the same
/// request as a portal call, and the toolkit's dialog when no portal answers.
///
/// GIO's D-Bus is used rather than a client library because it is already here
/// and already lives on the GTK main loop: a dialog stays open for as long as
/// a person takes, and nothing may block while it does.
mod portal {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use gtk::prelude::*;
    use gtk::{gio, glib};

    const PORTAL: &str = "org.freedesktop.portal.Desktop";
    const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
    const FILE_CHOOSER: &str = "org.freedesktop.portal.FileChooser";
    const REQUEST: &str = "org.freedesktop.portal.Request";
    /// The person chose something.
    const SUCCESS: u32 = 0;
    /// The person closed the dialog.
    const CANCELLED: u32 = 1;

    thread_local! {
        static TOKENS: Tokens = Tokens::new();
    }

    /// Request tokens, unique within the process: each copy of this crate (a
    /// module loaded into a host links its own) counts from zero on the
    /// same bus connection, so its tokens carry a random mark of its own.
    pub(super) struct Tokens {
        mark: u32,
        next: Cell<u64>,
    }

    impl Tokens {
        pub(super) fn new() -> Self {
            Self {
                mark: glib::random_int(),
                next: Cell::new(0),
            }
        }

        pub(super) fn next(&self) -> String {
            let count = self.next.get().wrapping_add(1);
            self.next.set(count);
            format!("big{:08x}_{count}", self.mark)
        }
    }

    /// Ask the portal, calling `answered` once with the URIs, or `None` when no
    /// portal answered and the caller should fall back.
    pub(super) fn ask(
        method: &'static str,
        parent: &str,
        title: &str,
        options: glib::Variant,
        answered: impl Fn(Option<Vec<gio::File>>) + 'static,
    ) {
        let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
            answered(None);
            return;
        };
        let Some(token) = handle_token(&bus) else {
            answered(None);
            return;
        };
        let (path, token) = token;

        // Subscribed before the call, not after: the portal may answer a
        // request the person had already dealt with before this returns, and a
        // signal nobody is listening for is simply lost. The subscription is a
        // guard, and holding it is what waiting means: whichever ends the
        // request first (the answer, a refused call, the portal leaving the
        // bus) drops it and answers.
        let subscription = Rc::new(RefCell::new(None));
        let watcher = Rc::new(Cell::new(None));
        let finish = {
            let (subscription, watcher) = (Rc::clone(&subscription), Rc::clone(&watcher));
            Rc::new(move |answer: Option<Vec<gio::File>>| {
                if subscription.borrow_mut().take().is_none() {
                    return;
                }
                if let Some(watcher) = watcher.take() {
                    gio::bus_unwatch_name(watcher);
                }
                answered(answer);
            })
        };
        let guard = bus.subscribe_to_signal(
            Some(PORTAL),
            Some(REQUEST),
            Some("Response"),
            Some(&path),
            None,
            gio::DBusSignalFlags::NONE,
            {
                let finish = Rc::clone(&finish);
                move |signal: gio::DBusSignalRef<'_>| finish(response(signal.parameters))
            },
        );
        subscription.borrow_mut().replace(guard);

        let arguments = glib::Variant::tuple_from_iter([
            parent.to_variant(),
            title.to_variant(),
            with_token(&options, &token),
        ]);
        let connection = bus.clone();
        bus.call(
            Some(PORTAL),
            PORTAL_PATH,
            FILE_CHOOSER,
            method,
            Some(&arguments),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |result| match result {
                // Watched from the reply on, when the name has its owner: a
                // portal that crashes with the dialog open never answers.
                Ok(_) if subscription.borrow().is_some() => {
                    let vanished = Rc::clone(&finish);
                    watcher.set(Some(gio::bus_watch_name_on_connection(
                        &connection,
                        PORTAL,
                        gio::BusNameWatcherFlags::NONE,
                        |_, _, _| {},
                        move |_, _| vanished(None),
                    )));
                }
                Ok(_) => {}
                Err(error) => {
                    // No portal on this desktop, or it refused the call. Either
                    // way the caller still needs a dialog, so say "not answered"
                    // rather than "no file".
                    log::debug!("the file portal did not take the request: {error}");
                    finish(None);
                }
            },
        );
    }

    /// The object path the portal will answer on, and the token that picks it.
    ///
    /// The portal derives the path from our bus name, so predicting it is what
    /// lets the subscription exist before the call does.
    fn handle_token(bus: &gio::DBusConnection) -> Option<(String, String)> {
        let unique = bus.unique_name()?;
        let sender = unique.trim_start_matches(':').replace('.', "_");
        let token = TOKENS.with(Tokens::next);
        Some((format!("{PORTAL_PATH}/request/{sender}/{token}"), token))
    }

    fn with_token(options: &glib::Variant, token: &str) -> glib::Variant {
        let dict = glib::VariantDict::new(Some(options));
        dict.insert("handle_token", token);
        dict.end()
    }

    /// The chosen files, none when the person cancelled, or `None` when the
    /// portal's dialog failed and the toolkit's should ask instead.
    pub(super) fn response(body: &glib::Variant) -> Option<Vec<gio::File>> {
        let (response, results) = body.get::<(u32, glib::VariantDict)>()?;
        match response {
            SUCCESS => Some(
                results
                    .lookup_value("uris", None)
                    .and_then(|uris| uris.get::<Vec<String>>())
                    .unwrap_or_default()
                    .iter()
                    .map(|uri| gio::File::for_uri(uri))
                    .collect(),
            ),
            CANCELLED => Some(Vec::new()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::*;

    /// Two copies of the crate in one process, each counting from zero,
    /// never ask the portal with the same token.
    #[test]
    fn fresh_counters_give_distinct_tokens() {
        let (first, second) = (portal::Tokens::new(), portal::Tokens::new());
        assert_ne!(first.next(), second.next());
        assert_ne!(first.next(), first.next(), "and one copy never repeats");
    }

    /// The portal starts in the folder of the file to select, and is given
    /// none for a bare file name.
    #[test]
    fn portal_starts_in_selected_folder() {
        let start = |select: &str| {
            let mut request = FileRequest::open_file();
            request.select = Some(select.into());
            glib::VariantDict::new(Some(&portal_options(&request)))
                .lookup_value("current_folder", None)
                .map(|folder| folder.fixed_array::<u8>().unwrap().to_vec())
        };
        assert_eq!(start("/home/ana/notes.txt"), Some(b"/home/ana\0".to_vec()));
        assert_eq!(start("notes.txt"), None);
    }

    /// Responses as the portal's `org.freedesktop.portal.Request` documents
    /// them: 0 chose, 1 cancelled, 2 ended another way.
    #[test]
    fn portal_response_reads_its_code() {
        let body = |code: u32| {
            let results = glib::VariantDict::new(None);
            results.insert("uris", vec!["file:///a"]);
            (code, results).to_variant()
        };
        let uris = |answer: Option<Vec<gio::File>>| {
            answer.map(|files| {
                files
                    .iter()
                    .map(|file| file.uri().to_string())
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(
            uris(portal::response(&body(0))),
            Some(vec!["file:///a".to_owned()])
        );
        assert_eq!(
            uris(portal::response(&body(1))),
            Some(Vec::new()),
            "cancelled"
        );
        assert_eq!(uris(portal::response(&body(2))), None, "failed");
        assert_eq!(uris(portal::response(&().to_variant())), None, "malformed");
    }

    /// A connection of its own on the session bus serving a `FileChooser`
    /// that takes `OpenFile`, sets `asked`, and never sends its response.
    fn portal_that_never_answers(asked: &Rc<Cell<bool>>) -> gio::DBusConnection {
        let address =
            gio::dbus_address_get_for_bus_sync(gio::BusType::Session, gio::Cancellable::NONE)
                .expect("a session bus");
        let portal = gio::DBusConnection::for_address_sync(
            &address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
            gio::Cancellable::NONE,
        )
        .expect("a connection of the portal's own");
        let node = gio::DBusNodeInfo::for_xml(
            r#"<node><interface name="org.freedesktop.portal.FileChooser">
              <method name="OpenFile">
                <arg type="s" direction="in"/><arg type="s" direction="in"/>
                <arg type="a{sv}" direction="in"/><arg type="o" direction="out"/>
              </method>
            </interface></node>"#,
        )
        .expect("valid introspection");
        let interface = node
            .lookup_interface("org.freedesktop.portal.FileChooser")
            .expect("declared");
        let taken = Rc::clone(asked);
        portal
            .register_object("/org/freedesktop/portal/desktop", &interface)
            .method_call(move |_, _, _, _, _, _, invocation| {
                taken.set(true);
                let handle = glib::variant::ObjectPath::try_from(
                    "/org/freedesktop/portal/desktop/request/unanswered",
                )
                .expect("an object path");
                invocation.return_value(Some(&(handle,).to_variant()));
            })
            .build()
            .expect("registered");
        portal
    }

    /// A portal that takes the request and leaves the bus without answering
    /// hands it to the toolkit's dialog rather than leaving it waiting.
    #[test]
    #[ignore = "owns the portal's name: needs the private bus of tools/headless-gate.sh"]
    fn portal_leaving_unanswered_falls_back() {
        let context = glib::MainContext::new();
        let turn_until = |done: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !done() && std::time::Instant::now() < deadline {
                if !context.iteration(false) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            done()
        };
        context
            .with_thread_default(|| {
                let asked = Rc::new(Cell::new(false));
                let portal = portal_that_never_answers(&asked);
                let owned = Rc::new(Cell::new(None));
                let (acquired, lost) = (Rc::clone(&owned), Rc::clone(&owned));
                let owner = gio::bus_own_name_on_connection(
                    &portal,
                    "org.freedesktop.portal.Desktop",
                    gio::BusNameOwnerFlags::DO_NOT_QUEUE,
                    move |_, _| acquired.set(Some(true)),
                    move |_, _| lost.set(Some(false)),
                );
                assert!(turn_until(&|| owned.get().is_some()));
                assert_eq!(
                    owned.get(),
                    Some(true),
                    "the portal's name is free on this bus"
                );

                let answer = Rc::new(RefCell::new(None));
                let answered = Rc::clone(&answer);
                portal::ask(
                    "OpenFile",
                    "",
                    "Open",
                    glib::VariantDict::new(None).end(),
                    move |files| {
                        answered
                            .borrow_mut()
                            .replace(files.map(|files| files.len()));
                    },
                );
                assert!(
                    turn_until(&|| asked.get()),
                    "the request reached the portal"
                );
                // Unowned first: gio-rs unwraps the connection GIO hands a
                // closed one's `name_lost` as NULL.
                gio::bus_unown_name(owner);
                portal.close_sync(gio::Cancellable::NONE).expect("closed");

                assert!(turn_until(&|| answer.borrow().is_some()), "an answer");
                assert_eq!(*answer.borrow(), Some(None), "not answered: fall back");
            })
            .expect("the context of this thread");
    }

    #[test]
    fn dismissing_is_no_file() {
        for code in [gtk::DialogError::Dismissed, gtk::DialogError::Cancelled] {
            let answer = one(Err(glib::Error::new(code, "closed")));
            assert!(answer.is_ok_and(|files| files.is_empty()));
        }
        let failed = one(Err(glib::Error::new(gtk::DialogError::Failed, "broken")));
        assert_eq!(
            failed.map_err(|error| error.message().to_owned()),
            Err("broken".to_owned())
        );
        let remote = gio::File::for_uri("sftp://example.invalid/a.txt");
        let kept = one(Ok(remote.clone())).expect("a chosen file");
        assert_eq!(kept.first().map(gio::File::uri), Some(remote.uri()));
    }

    #[test]
    fn local_paths_keep_local_files() {
        let local = gio::File::for_path("/srv/a b.txt");
        let remote = gio::File::for_uri("sftp://example.invalid/a.txt");
        assert_eq!(
            local_paths(Ok(vec![local, remote])),
            [PathBuf::from("/srv/a b.txt")]
        );
        assert!(local_paths(Ok(Vec::new())).is_empty());
        let failed = glib::Error::new(gtk::DialogError::Failed, "broken");
        assert!(local_paths(Err(failed)).is_empty());
    }

    /// GTK's own suffix filter matches the same names: an oracle for the glob.
    #[test]
    fn suffix_globs_match_any_case() {
        assert_eq!(suffix_glob("mp3"), "*.[mM][pP]3");
        assert_eq!(suffix_glob("tar.gz"), "*.[tT][aA][rR].[gG][zZ]");
        assert_eq!(suffix_glob("a*b"), "*.[aA][*][bB]");
        let request = FileRequest::open_files().offering_suffixes("Audio", &["mp3", "ogg"]);
        assert_eq!(request.types[0].patterns, ["*.[mM][pP]3", "*.[oO][gG][gG]"]);
    }

    #[test]
    fn portal_filters_keep_rule_kinds() {
        // The rule kind is a bare number on the wire, and getting it wrong
        // gives a dialog that silently matches nothing.
        let types = [FileType {
            label: "Text Files".to_owned(),
            patterns: vec!["*.txt".to_owned()],
            mime_types: vec!["text/plain".to_owned()],
        }];
        assert_eq!(
            portal_filters(&types).to_string(),
            "[('Text Files', [(uint32 0, '*.txt'), (1, 'text/plain')])]"
        );
    }
}
