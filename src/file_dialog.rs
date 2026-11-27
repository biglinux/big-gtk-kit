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
    /// A file whose folder to start in when `start_in` names none. Neither
    /// dialog selects it: an open dialog takes a folder, not a file.
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

    /// Start in the folder of `file`, unless [`Self::starting_in`] names one.
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
/// filter for the portal: `mp3` gives `*.[mM][pP]3`, and `ß`, whose
/// uppercase `SS` no bracket can hold, stays `ß`.
fn suffix_glob(suffix: &str) -> String {
    let mut glob = String::from("*.");
    for character in suffix.chars() {
        let lower = character.to_lowercase().collect::<String>();
        let upper = character.to_uppercase().collect::<String>();
        if lower == upper || lower.chars().count() > 1 || upper.chars().count() > 1 {
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
                    Some(picked) => answer_once(picked),
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
    // Resolved here as the toolkit's dialog resolves it: the portal runs in a
    // working directory of its own.
    if let Some(start) = start.and_then(|start| gio::File::for_path(start).path()) {
        // Paths travel as NUL-terminated bytes, not as text: a file name is
        // bytes on Linux and need not be UTF-8.
        let mut bytes = start.into_os_string().into_encoded_bytes();
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
    // The file first: it sets the folder too, and `start_in` outranks that
    // folder, as it does for the portal.
    if let Some(file) = &request.select {
        dialog.set_initial_file(Some(&gio::File::for_path(file)));
    }
    if let Some(start) = &request.start_in {
        dialog.set_initial_folder(Some(&gio::File::for_path(start)));
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

    /// Ask the portal, calling `answered` once with the chosen files or its
    /// failure, or `None` when no portal is there to finish the request: GTK's
    /// dialog, which asks instead, goes to the same portal whenever there is one.
    pub(super) fn ask(
        method: &'static str,
        parent: &str,
        title: &str,
        options: glib::Variant,
        answered: impl Fn(Option<Result<Vec<gio::File>, glib::Error>>) + 'static,
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
            Rc::new(move |answer: Option<Result<Vec<gio::File>, glib::Error>>| {
                if subscription.borrow_mut().take().is_none() {
                    return;
                }
                if let Some(watcher) = watcher.take() {
                    gio::bus_unwatch_name(watcher);
                }
                answered(answer);
            })
        };
        let listen = {
            let (bus, finish) = (bus.clone(), Rc::clone(&finish));
            move |path: &str| {
                let finish = Rc::clone(&finish);
                bus.subscribe_to_signal(
                    Some(PORTAL),
                    Some(REQUEST),
                    Some("Response"),
                    Some(path),
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |signal: gio::DBusSignalRef<'_>| {
                        finish(Some(response(signal.parameters)));
                    },
                )
            }
        };
        subscription.borrow_mut().replace(listen(&path));

        let arguments = glib::Variant::tuple_from_iter([
            parent.to_variant(),
            title.to_variant(),
            with_token(&options, &token),
        ]);
        let call = gio::DBusMessage::new_method_call(
            Some(PORTAL),
            PORTAL_PATH,
            Some(FILE_CHOOSER),
            method,
        );
        call.set_body(&arguments);
        let connection = bus.clone();
        bus.send_message_with_reply(
            &call,
            gio::DBusSendMessageFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |reply| match reply.and_then(carried_error) {
                // Watched from the reply on, by the unique name that sent it: a
                // portal that crashes with the dialog open never answers, and
                // one restarted under the same name knows nothing of this
                // request. A name already gone is reported as vanished.
                Ok(reply) if subscription.borrow().is_some() => {
                    let handle = reply
                        .body()
                        .and_then(|body| body.get::<(glib::variant::ObjectPath,)>());
                    let (Some(portal), Some((handle,))) = (reply.sender(), handle) else {
                        finish(Some(Err(glib::Error::new(
                            gtk::DialogError::Failed,
                            "the file portal sent a malformed reply",
                        ))));
                        return;
                    };
                    // A portal older than `handle_token` answers on a path of
                    // its own choosing. An answer it sent there before this
                    // line is lost, as the portal's documentation accepts.
                    if handle.as_str() != path {
                        let moved = listen(handle.as_str());
                        subscription.borrow_mut().replace(moved);
                    }
                    let vanished = Rc::clone(&finish);
                    watcher.set(Some(gio::bus_watch_name_on_connection(
                        &connection,
                        &portal,
                        gio::BusNameWatcherFlags::NONE,
                        |_, _, _| {},
                        move |_, _| vanished(None),
                    )));
                }
                Ok(_) => {}
                Err(error) if absent(&error) => {
                    log::debug!("the file portal did not take the request: {error}");
                    finish(None);
                }
                Err(error) => finish(Some(Err(error))),
            },
        );
    }

    /// The reply, or the error it carries, as GLib's `g_dbus_message_to_gerror`
    /// reads it. gio-rs 0.22's `DBusMessage::to_gerror` asserts the inverse of
    /// that function's return value, so it panics in every debug build.
    fn carried_error(reply: gio::DBusMessage) -> Result<gio::DBusMessage, glib::Error> {
        if reply.message_type() != gio::DBusMessageType::Error {
            return Ok(reply);
        }
        let message = reply
            .body()
            .and_then(|body| body.get::<(String,)>())
            .map(|(message,)| message)
            .unwrap_or_default();
        Err(gio::DBusError::new_for_dbus_error(
            reply.error_name().as_deref().unwrap_or_default(),
            &message,
        ))
    }

    /// A refusal meaning this bus has no `FileChooser` portal, which GTK's own
    /// check finds too before it shows its dialog instead.
    fn absent(error: &glib::Error) -> bool {
        [
            gio::DBusError::ServiceUnknown,
            gio::DBusError::UnknownObject,
            gio::DBusError::UnknownInterface,
            gio::DBusError::UnknownMethod,
        ]
        .into_iter()
        .any(|code| error.matches(code))
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

    /// The chosen files, none when the person cancelled, or the failure of the
    /// portal's dialog.
    pub(super) fn response(body: &glib::Variant) -> Result<Vec<gio::File>, glib::Error> {
        let failed = |message: &str| glib::Error::new(gtk::DialogError::Failed, message);
        let Some((response, results)) = body.get::<(u32, glib::VariantDict)>() else {
            return Err(failed("the file portal sent a malformed response"));
        };
        match response {
            SUCCESS => match results.lookup::<Vec<String>>("uris") {
                Ok(Some(uris)) => Ok(uris.iter().map(|uri| gio::File::for_uri(uri)).collect()),
                _ => Err(failed("the file portal chose without naming the files")),
            },
            CANCELLED => Ok(Vec::new()),
            code => Err(failed(&format!(
                "the file portal's dialog ended with response {code}"
            ))),
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

    /// A relative folder reaches the portal, which has a working directory
    /// of its own, as the folder the toolkit's dialog starts in.
    #[test]
    fn portal_folder_resolves_relative() {
        let request = FileRequest::open_file().starting_in("music");
        let folder = glib::VariantDict::new(Some(&portal_options(&request)))
            .lookup_value("current_folder", None)
            .map(|folder| folder.fixed_array::<u8>().unwrap().to_vec());
        let mut expected = gio::File::for_path("music")
            .path()
            .expect("a local path")
            .into_os_string()
            .into_encoded_bytes();
        expected.push(0);
        assert!(expected.starts_with(b"/"));
        assert_eq!(folder, Some(expected));
    }

    /// The toolkit's dialog starts where the portal's does: in `start_in`
    /// over the selected file's folder, which still names the saved file.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn start_folder_outranks_selection() {
        gtk::init().expect("a display");
        let request = FileRequest::save_file("")
            .starting_in("/srv/music")
            .selecting("/home/ana/song.ogg");
        let dialog = build(&request);
        assert_eq!(
            dialog.initial_folder().and_then(|folder| folder.path()),
            Some(PathBuf::from("/srv/music"))
        );
        assert_eq!(dialog.initial_name().as_deref(), Some("song.ogg"));
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
        let uris = |answer: Result<Vec<gio::File>, glib::Error>| {
            answer.ok().map(|files| {
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
        assert!(portal::response(&body(2)).is_err(), "failed");
        assert!(portal::response(&().to_variant()).is_err(), "malformed");

        let chose = |uris: Option<glib::Variant>| {
            let results = glib::VariantDict::new(None);
            if let Some(uris) = uris {
                results.insert_value("uris", &uris);
            }
            portal::response(&(0u32, results).to_variant())
        };
        assert!(chose(None).is_err(), "chose without uris");
        assert!(
            chose(Some("file:///a".to_variant())).is_err(),
            "uris not a list"
        );
    }

    /// Turn this thread's context until `done`, for at most five seconds.
    fn turn_until(done: &dyn Fn() -> bool) -> bool {
        let context = glib::MainContext::ref_thread_default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done() && std::time::Instant::now() < deadline {
            if !context.iteration(false) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        done()
    }

    /// What [`portal::ask`] answered, once it has.
    type Answer = Rc<RefCell<Option<Option<Result<Vec<gio::File>, glib::Error>>>>>;

    /// Ask this bus's portal for a file to open.
    fn ask_open() -> Answer {
        let answer = Answer::default();
        let answered = Rc::clone(&answer);
        portal::ask(
            "OpenFile",
            "",
            "Open",
            glib::VariantDict::new(None).end(),
            move |files| {
                answered.borrow_mut().replace(files);
            },
        );
        answer
    }

    /// A connection of its own on the session bus, apart from the shared one
    /// [`portal::ask`] calls on.
    fn own_connection() -> gio::DBusConnection {
        let address =
            gio::dbus_address_get_for_bus_sync(gio::BusType::Session, gio::Cancellable::NONE)
                .expect("a session bus");
        gio::DBusConnection::for_address_sync(
            &address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
            gio::Cancellable::NONE,
        )
        .expect("a connection of its own")
    }

    /// A connection of its own, owning the portal's name and serving a
    /// `FileChooser` whose `OpenFile` answers through `reply`.
    fn fake_portal(
        reply: impl Fn(gio::DBusMethodInvocation) + 'static,
    ) -> (gio::DBusConnection, gio::OwnerId) {
        let portal = own_connection();
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
        portal
            .register_object("/org/freedesktop/portal/desktop", &interface)
            .method_call(move |_, _, _, _, _, _, invocation| reply(invocation))
            .build()
            .expect("registered");
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
        (portal, owner)
    }

    /// An `OpenFile` that takes the request, sets `asked`, and names a
    /// request handle other than the one the token asked for.
    fn take_request(asked: &Rc<Cell<bool>>) -> impl Fn(gio::DBusMethodInvocation) + 'static {
        let asked = Rc::clone(asked);
        move |invocation| {
            asked.set(true);
            let handle = glib::variant::ObjectPath::try_from(
                "/org/freedesktop/portal/desktop/request/unanswered",
            )
            .expect("an object path");
            invocation.return_value(Some(&(handle,).to_variant()));
        }
    }

    /// A portal that takes the request and leaves the bus without answering
    /// hands it to the toolkit's dialog rather than leaving it waiting.
    #[test]
    #[ignore = "owns the portal's name: needs the private bus of tools/headless-gate.sh"]
    fn portal_leaving_unanswered_falls_back() {
        glib::MainContext::new()
            .with_thread_default(|| {
                let asked = Rc::new(Cell::new(false));
                let (portal, owner) = fake_portal(take_request(&asked));
                let answer = ask_open();
                assert!(
                    turn_until(&|| asked.get()),
                    "the request reached the portal"
                );
                // The reply on the bus before the portal leaves it, so the call
                // succeeds and only the name watch can answer: a reply lost
                // with the connection is an error, never a fallback.
                portal.flush_sync(gio::Cancellable::NONE).expect("flushed");
                // Unowned first: gio-rs unwraps the connection GIO hands a
                // closed one's `name_lost` as NULL.
                gio::bus_unown_name(owner);
                portal.close_sync(gio::Cancellable::NONE).expect("closed");

                assert!(turn_until(&|| answer.borrow().is_some()), "an answer");
                assert!(
                    matches!(*answer.borrow(), Some(None)),
                    "not answered: fall back"
                );
            })
            .expect("the context of this thread");
    }

    /// A connection of its own that owns the portal's name, taken with
    /// synchronous calls, which leave this thread's context, and any reply
    /// waiting there, unread.
    fn successor_portal() -> gio::DBusConnection {
        let successor = own_connection();
        let bus_call = |method: &str, arguments: glib::Variant| {
            successor
                .call_sync(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                    method,
                    Some(&arguments),
                    None,
                    gio::DBusCallFlags::NONE,
                    -1,
                    gio::Cancellable::NONE,
                )
                .ok()
        };
        bus_call(
            "RequestName",
            ("org.freedesktop.portal.Desktop", 0u32).to_variant(),
        )
        .expect("in line for the portal's name");
        let owner = || {
            bus_call(
                "GetNameOwner",
                ("org.freedesktop.portal.Desktop",).to_variant(),
            )
            .and_then(|owner| owner.get::<(String,)>())
            .map(|(owner,)| owner)
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while owner() != successor.unique_name().map(String::from) {
            assert!(
                std::time::Instant::now() < deadline,
                "the successor owns the name"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        successor
    }

    /// A portal restarted under the same name before the reply is read knows
    /// nothing of the request, so the one that took it is what is watched.
    #[test]
    #[ignore = "owns the portal's name: needs the private bus of tools/headless-gate.sh"]
    fn portal_replaced_unanswered_falls_back() {
        glib::MainContext::new()
            .with_thread_default(|| {
                let asked = Rc::new(Cell::new(false));
                let (portal, owner) = fake_portal(take_request(&asked));
                let answer = ask_open();
                assert!(
                    turn_until(&|| asked.get()),
                    "the request reached the portal"
                );
                portal.flush_sync(gio::Cancellable::NONE).expect("flushed");
                gio::bus_unown_name(owner);
                portal.close_sync(gio::Cancellable::NONE).expect("closed");

                let _successor = successor_portal();

                assert!(turn_until(&|| answer.borrow().is_some()), "an answer");
                assert!(
                    matches!(*answer.borrow(), Some(None)),
                    "not answered: fall back"
                );
            })
            .expect("the context of this thread");
    }

    /// A portal that answers on a handle of its own rather than the one
    /// `handle_token` asked for, as one older than that option does, is heard.
    #[test]
    #[ignore = "owns the portal's name: needs the private bus of tools/headless-gate.sh"]
    fn portal_own_handle_is_heard() {
        glib::MainContext::new()
            .with_thread_default(|| {
                let asked = Rc::new(Cell::new(false));
                let (portal, _owner) = fake_portal(take_request(&asked));
                let answer = ask_open();
                let results = glib::VariantDict::new(None);
                results.insert("uris", vec!["file:///a"]);
                let response = (0u32, results).to_variant();
                // Sent until heard: nothing tells the test when the asker has
                // moved its subscription to the handle the portal named.
                let heard = turn_until(&|| {
                    if asked.get() {
                        portal
                            .emit_signal(
                                None,
                                "/org/freedesktop/portal/desktop/request/unanswered",
                                "org.freedesktop.portal.Request",
                                "Response",
                                Some(&response),
                            )
                            .expect("sent");
                    }
                    answer.borrow().is_some()
                });
                assert!(heard, "an answer");
                assert!(
                    matches!(
                        &*answer.borrow(),
                        Some(Some(Ok(files)))
                            if files.iter().map(gio::File::uri).eq(["file:///a"])
                    ),
                    "the file the portal chose"
                );
            })
            .expect("the context of this thread");
    }

    /// No portal on the bus leaves the asking to the toolkit's dialog.
    #[test]
    #[ignore = "asks for the portal's name: needs the private bus of tools/headless-gate.sh"]
    fn missing_portal_falls_back() {
        glib::MainContext::new()
            .with_thread_default(|| {
                let answer = ask_open();
                assert!(turn_until(&|| answer.borrow().is_some()), "an answer");
                assert!(matches!(*answer.borrow(), Some(None)), "fall back");
            })
            .expect("the context of this thread");
    }

    /// A portal that refuses the request is a failed dialog: the toolkit's
    /// dialog would ask that same portal again.
    #[test]
    #[ignore = "owns the portal's name: needs the private bus of tools/headless-gate.sh"]
    fn portal_refusal_is_error() {
        glib::MainContext::new()
            .with_thread_default(|| {
                let _portal = fake_portal(|invocation| {
                    invocation.return_dbus_error("org.freedesktop.DBus.Error.AccessDenied", "no");
                });
                let answer = ask_open();
                assert!(turn_until(&|| answer.borrow().is_some()), "an answer");
                assert!(
                    matches!(
                        &*answer.borrow(),
                        Some(Some(Err(error))) if error.matches(gio::DBusError::AccessDenied)
                    ),
                    "the portal's own error"
                );
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
        // GLib's one-character mapping, which GTK's glob uses: `ß` has no
        // uppercase of its own, `ẞ` lowercases to `ß`.
        assert_eq!(suffix_glob("straße"), "*.[sS][tT][rR][aA]ß[eE]");
        assert_eq!(suffix_glob("ẞ"), "*.[ßẞ]");
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
