// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Typed settings documents and RAII change streams.
//!
//! Apps own the concrete serde document type and place it inside
//! `BigSettingsDocument` (the `background-save` feature). The document
//! persists through [`crate::storage::BigVersionedJsonStore`] on a GIO worker
//! and emits key-scoped change notifications on the GTK main context. The
//! substrate is single-threaded: clone handles freely inside the main context,
//! but do not send them to worker threads.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

#[cfg(feature = "background-save")]
use serde::{Serialize, de::DeserializeOwned};

#[cfg(feature = "background-save")]
use crate::storage::{BigStorageError, BigVersionedJsonStore};

/// Resolve the XDG config home directory for app-owned configuration files.
///
/// Uses `$XDG_CONFIG_HOME` when set, falls back to `$HOME/.config`, then to
/// `.config` in the home folder the user database names. A process with none
/// of those gets `/.config`, where an ordinary user's writes fail instead of
/// landing in whatever directory it was started from.
///
/// # Examples
///
/// ```
/// use big_os_kit::settings::config_home;
///
/// let path = config_home();
/// assert!(!path.as_os_str().is_empty());
/// ```
#[must_use]
pub fn config_home() -> PathBuf {
    config_home_checked().unwrap_or_else(|| account_home().join(".config"))
}

/// Where this user's caches live.
///
/// Same rules as [`config_home`], for `XDG_CACHE_HOME` and `~/.cache`.
#[must_use]
pub fn cache_home() -> PathBuf {
    cache_home_checked().unwrap_or_else(|| account_home().join(".cache"))
}

/// The home folder the user database names for this process's user, or `/`.
fn account_home() -> PathBuf {
    crate::users::home_of(crate::users::own_uid())
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Where this user's settings live, or nothing when neither variable names an
/// absolute path.
///
/// Code that would rather do without than ask the user database, such as a
/// cache it can skip, takes this instead of [`config_home`].
#[must_use]
pub fn config_home_checked() -> Option<PathBuf> {
    xdg_dir(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
        ".config",
    )
}

/// Where this user's caches live, or nothing. See [`config_home_checked`].
#[must_use]
pub fn cache_home_checked() -> Option<PathBuf> {
    xdg_dir(
        std::env::var_os("XDG_CACHE_HOME"),
        std::env::var_os("HOME"),
        ".cache",
    )
}

/// Resolve one XDG base directory from explicit values.
///
/// The arguments make the rules testable, and let a caller with its own
/// fallback keep it: `xdg_dir(...).unwrap_or_else(std::env::temp_dir)`.
///
/// A relative value is ignored rather than resolved, as the XDG basedir
/// specification requires: a stray `XDG_CONFIG_HOME=config` would otherwise
/// scatter settings through whatever directory the program was started in.
#[must_use]
pub fn xdg_dir(
    xdg_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    fallback_name: &str,
) -> Option<PathBuf> {
    xdg_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(fallback_name))
        })
}

type SettingListener = Rc<dyn Fn(&str)>;

/// Main-context settings change fan-out keyed by setting id.
///
/// Subscriptions return a [`BigSettingChangeSubscription`]. Dropping that token
/// removes the callback from the listener list, so consumers cannot accidentally
/// leave stale UI closures attached forever.
#[derive(Clone, Default)]
pub struct BigSettingChangeStream {
    inner: Rc<BigSettingChangeStreamInner>,
}

#[derive(Default)]
struct BigSettingChangeStreamInner {
    next_listener_id: Cell<u64>,
    listeners: RefCell<BTreeMap<u64, SettingListener>>,
}

impl BigSettingChangeStream {
    /// Subscribe to setting-key changes.
    ///
    /// Keep the returned token for as long as the callback should stay active.
    /// Dropping the token unsubscribes.
    #[must_use = "dropping BigSettingChangeSubscription unsubscribes the settings listener"]
    pub fn subscribe(&self, listener: impl Fn(&str) + 'static) -> BigSettingChangeSubscription {
        self.subscribe_listener(Rc::new(listener))
    }

    /// Subscribe with a pre-built callback object.
    ///
    /// This object-safe entry point lets UI traits pass listeners through
    /// `Rc<dyn Fn(&str)>` without adding their own subscription layer.
    #[must_use = "dropping BigSettingChangeSubscription unsubscribes the settings listener"]
    pub fn subscribe_listener(&self, listener: SettingListener) -> BigSettingChangeSubscription {
        let id = self.inner.next_listener_id.get();
        self.inner.next_listener_id.set(id.wrapping_add(1));
        self.inner.listeners.borrow_mut().insert(id, listener);
        BigSettingChangeSubscription {
            id,
            stream: Rc::downgrade(&self.inner),
        }
    }

    /// Notify listeners that `key` changed.
    pub fn emit(&self, key: &str) {
        let listeners: Vec<_> = self.inner.listeners.borrow().values().cloned().collect();
        for listener in listeners {
            listener(key);
        }
    }
}

/// RAII subscription token for [`BigSettingChangeStream`].
///
/// The token is `#[must_use]`: if callers ignore it, the
/// subscription is immediately dropped and no callback remains registered.
#[must_use = "dropping BigSettingChangeSubscription unsubscribes the settings listener"]
pub struct BigSettingChangeSubscription {
    id: u64,
    stream: Weak<BigSettingChangeStreamInner>,
}

impl Drop for BigSettingChangeSubscription {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.upgrade() {
            stream.listeners.borrow_mut().remove(&self.id);
        }
    }
}

/// App-owned typed settings document persisted through a versioned JSON store.
///
/// `T` is the app's serde struct. All mutation goes through [`Self::apply`]
/// or [`Self::apply_change`], which change the document and emit the changed
/// key at once, then write the file on a GIO worker.
///
/// # Examples
///
/// ```
/// use big_os_kit::settings::BigSettingsDocument;
/// use big_os_kit::storage::BigVersionedJsonStore;
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
/// struct AppSettings {
///     follow_system: bool,
/// }
///
/// let path = std::env::temp_dir().join(format!(
///     "big-settings-document-doctest-{}.json",
///     std::process::id(),
/// ));
/// let store = BigVersionedJsonStore::new(&path, 1);
/// let document = BigSettingsDocument::new(AppSettings::default(), store);
/// let _token = document.subscribe(|key| assert_eq!(key, "follow-system"));
/// document.apply_change(
///     "follow-system",
///     |settings| settings.follow_system = true,
///     |error| eprintln!("settings not saved: {error}"),
/// );
/// assert!(document.snapshot().follow_system);
/// let _ = std::fs::remove_file(path);
/// ```
#[cfg(feature = "background-save")]
#[derive(Clone)]
pub struct BigSettingsDocument<T> {
    inner: Rc<BigSettingsDocumentInner<T>>,
}

#[cfg(feature = "background-save")]
struct BigSettingsDocumentInner<T> {
    document: RefCell<T>,
    store: BigVersionedJsonStore<T>,
    /// False when the file could not be read: it is left for whoever can.
    saves: Cell<bool>,
    changes: BigSettingChangeStream,
    /// A background write is running; [`BigSettingsDocument::apply`] marks
    /// `save_again` instead of starting a second one.
    saving: Cell<bool>,
    save_again: Cell<bool>,
    on_save_failed: RefCell<Option<SaveReport>>,
}

/// Where a failed background write is said.
#[cfg(feature = "background-save")]
type SaveReport = Rc<dyn Fn(&BigStorageError)>;

#[cfg(feature = "background-save")]
impl<T> BigSettingsDocument<T> {
    /// Create a document from an already resolved value and store.
    #[must_use]
    pub fn new(document: T, store: BigVersionedJsonStore<T>) -> Self {
        Self {
            inner: Rc::new(BigSettingsDocumentInner {
                document: RefCell::new(document),
                store,
                saves: Cell::new(true),
                changes: BigSettingChangeStream::default(),
                saving: Cell::new(false),
                save_again: Cell::new(false),
                on_save_failed: RefCell::new(None),
            }),
        }
    }

    /// Borrow the document for a synchronous main-context read.
    pub fn read<R>(&self, read_document: impl FnOnce(&T) -> R) -> R {
        read_document(&self.inner.document.borrow())
    }

    /// Return a clone of the current document value.
    #[must_use]
    pub fn snapshot(&self) -> T
    where
        T: Clone,
    {
        self.inner.document.borrow().clone()
    }

    /// Return the change stream backing this document.
    #[must_use]
    pub fn change_stream(&self) -> BigSettingChangeStream {
        self.inner.changes.clone()
    }

    /// Subscribe to changes emitted by this document.
    #[must_use = "dropping BigSettingChangeSubscription unsubscribes the settings listener"]
    pub fn subscribe(&self, listener: impl Fn(&str) + 'static) -> BigSettingChangeSubscription {
        self.inner.changes.subscribe(listener)
    }
}

#[cfg(feature = "background-save")]
impl<T> BigSettingsDocument<T>
where
    T: Clone + Serialize + DeserializeOwned,
{
    /// Load the current document from `path`, or use `default_document` when
    /// the file is absent.
    ///
    /// # Errors
    ///
    /// Returns [`BigStorageError`] from the underlying versioned store when
    /// the file cannot be read, parsed, version-matched, or deserialized.
    pub(crate) fn load_or(
        path: impl Into<PathBuf>,
        current_version: u32,
        default_document: T,
    ) -> Result<Self, BigStorageError> {
        let store = BigVersionedJsonStore::new(path, current_version);
        let document = store.read()?.unwrap_or(default_document);
        Ok(Self::new(document, store))
    }

    /// Read `store`, or start from [`Default`] when it has no file yet.
    ///
    /// A file this build cannot read (damaged, too large, or written by a newer
    /// build) is left as it is: the defaults live in memory, changes still
    /// apply and notify, and nothing is written over the file.
    #[must_use]
    pub fn open(store: BigVersionedJsonStore<T>) -> Self
    where
        T: Default,
    {
        match store.read() {
            Ok(document) => Self::new(document.unwrap_or_default(), store),
            Err(error) => {
                log::warn!(
                    "{} is kept as it is and not saved over: {error}",
                    store.path().display()
                );
                let document = Self::new(T::default(), store);
                document.inner.saves.set(false);
                document
            }
        }
    }

    /// Load the current document from `path`, or use [`Default`] when absent.
    ///
    /// # Errors
    ///
    /// Returns [`BigStorageError`] from the underlying versioned store when
    /// the file cannot be read, parsed, version-matched, or deserialized.
    pub fn load_or_default(
        path: impl Into<PathBuf>,
        current_version: u32,
    ) -> Result<Self, BigStorageError>
    where
        T: Default,
    {
        Self::load_or(path, current_version, T::default())
    }
}

#[cfg(feature = "background-save")]
impl<T> BigSettingsDocument<T>
where
    T: Clone + Serialize + DeserializeOwned + Send + 'static,
{
    /// Whether a write runs or waits: a process that ends before it reads
    /// false can leave the write's temporary file beside the document.
    #[must_use]
    pub fn is_saving(&self) -> bool {
        self.inner.saving.get()
    }

    /// [`Self::apply`] on a draft that `change` edits.
    pub fn apply_change(
        &self,
        key: &'static str,
        change: impl FnOnce(&mut T),
        report: impl Fn(&BigStorageError) + 'static,
    ) {
        let mut draft = self.snapshot();
        change(&mut draft);
        self.apply(key, draft, report);
    }

    /// Replace the whole document and notify that `key` changed at once,
    /// then write it on a GIO worker. One write runs at a time, and changes
    /// made meanwhile are written together once it ends. A failed write
    /// keeps the document in memory and is said through the `report` of the
    /// latest change; the next change writes the file again.
    pub fn apply(
        &self,
        key: &'static str,
        document: T,
        report: impl Fn(&BigStorageError) + 'static,
    ) {
        self.inner.on_save_failed.replace(Some(Rc::new(report)));
        self.inner.document.replace(document);
        self.inner.changes.emit(key);
        if !self.inner.saves.get() {
            return;
        }
        if self.inner.saving.replace(true) {
            self.inner.save_again.set(true);
            return;
        }
        let inner = Rc::downgrade(&self.inner);
        glib::spawn_future_local(async move {
            loop {
                let Some(owner) = inner.upgrade() else {
                    return;
                };
                let store = owner.store.clone();
                let snapshot = owner.document.borrow().clone();
                drop(owner);
                let written = gio::spawn_blocking(move || store.write(&snapshot))
                    .await
                    .unwrap_or_else(|_| {
                        Err(BigStorageError::Io(std::io::Error::other(
                            "settings writer stopped",
                        )))
                    });
                let Some(owner) = inner.upgrade() else {
                    return;
                };
                if let Err(error) = written {
                    let report = owner.on_save_failed.borrow().clone();
                    if let Some(report) = report {
                        report(&error);
                    }
                }
                if !owner.save_again.replace(false) {
                    owner.saving.set(false);
                    return;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::disallowed_methods,
        reason = "tests use temp settings files and do not run on a UI main loop"
    )]

    use super::*;

    #[test]
    fn config_home_returns_non_empty_path() {
        assert!(!config_home().as_os_str().is_empty());
    }

    #[test]
    fn account_home_comes_from_user_database() {
        let home = account_home();
        assert!(home.is_absolute());
        assert_ne!(home, PathBuf::from("/"));
    }

    #[test]
    fn empty_config_home_uses_home() {
        assert_eq!(
            xdg_dir(
                Some(std::ffi::OsString::new()),
                Some("/home/test".into()),
                ".config"
            ),
            Some(PathBuf::from("/home/test/.config"))
        );
    }

    /// The specification says a relative value is invalid and must be ignored;
    /// resolving it would scatter settings through the working directory.
    #[test]
    fn a_relative_xdg_value_is_ignored() {
        assert_eq!(
            xdg_dir(Some("config".into()), Some("/home/test".into()), ".config"),
            Some(PathBuf::from("/home/test/.config"))
        );
        assert_eq!(xdg_dir(None, Some("relative".into()), ".cache"), None);
    }

    #[test]
    fn dropped_subscription_stops_listening() {
        let stream = BigSettingChangeStream::default();
        let calls = Rc::new(Cell::new(0));
        let token = stream.subscribe({
            let calls = Rc::clone(&calls);
            move |_| calls.set(calls.get() + 1)
        });
        stream.emit("level");

        drop(token);
        stream.emit("level");

        assert_eq!(calls.get(), 1);
    }

    #[cfg(feature = "background-save")]
    mod document {
        use super::*;
        use serde::Deserialize;
        use serde_json::json;
        use std::fs;
        use std::sync::atomic::{AtomicU64, Ordering};

        static SETTINGS_TEST_SEQ: AtomicU64 = AtomicU64::new(0);

        #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
        struct TestSettings {
            enabled: bool,
            level: f64,
        }

        fn settings_test_path(name: &str) -> PathBuf {
            std::env::temp_dir().join(format!(
                "big-os-kit-settings-test-{}-{}-{name}.json",
                std::process::id(),
                SETTINGS_TEST_SEQ.fetch_add(1, Ordering::Relaxed),
            ))
        }

        /// Turn the default main context until `done`, as an app's loop would.
        fn wait(done: impl Fn() -> bool) {
            let context = glib::MainContext::default();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !done() {
                assert!(std::time::Instant::now() < deadline, "the write must end");
                context.iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }

        /// A change shows at once and is written off the caller; a write that
        /// fails keeps the change and is reported.
        #[test]
        fn applied_change_saves_later() {
            let path = settings_test_path("apply");
            let document = BigSettingsDocument::new(
                TestSettings::default(),
                BigVersionedJsonStore::new(&path, 1),
            );
            for level in [1.0, 2.0, 3.0] {
                document.apply(
                    "level",
                    TestSettings {
                        enabled: true,
                        level,
                    },
                    |_| {},
                );
            }
            assert!((document.snapshot().level - 3.0).abs() < f64::EPSILON);
            // A change edits the document as it is when the change is made.
            document.apply_change("enabled", |settings| settings.enabled = false, |_| {});
            let held = document.snapshot();
            assert!(!held.enabled && (held.level - 3.0).abs() < f64::EPSILON);
            document.apply_change("level", |settings| settings.level = 3.0, |_| {});
            // Changes made during a write are written again after it, so the
            // file is removed only once no write is left.
            wait(|| !document.inner.saving.get());
            let store = BigVersionedJsonStore::<TestSettings>::new(&path, 1);
            assert_eq!(store.read().ok().flatten(), Some(held));
            let _ = fs::remove_file(&path);

            let blocked = settings_test_path("apply-blocked");
            fs::write(&blocked, b"a file, not a folder").unwrap();
            let document = BigSettingsDocument::new(
                TestSettings::default(),
                BigVersionedJsonStore::new(blocked.join("settings.json"), 1),
            );
            let failures = Rc::new(Cell::new(0));
            let counted = Rc::clone(&failures);
            document.apply(
                "enabled",
                TestSettings {
                    enabled: true,
                    level: 0.0,
                },
                move |_| counted.set(counted.get() + 1),
            );
            assert!(document.snapshot().enabled);
            wait(|| failures.get() == 1);
            let _ = fs::remove_file(&blocked);
        }

        /// A change made while a write runs is written once that write ends.
        #[test]
        fn change_during_write_is_written() {
            let context = glib::MainContext::default();
            let _owner = context
                .acquire()
                .expect("test thread owns the default context");
            let path = settings_test_path("apply-during-write");
            let document = BigSettingsDocument::new(
                TestSettings::default(),
                BigVersionedJsonStore::new(&path, 1),
            );
            let level = |level| TestSettings {
                enabled: true,
                level,
            };
            document.apply("level", level(1.0), |_| {});
            // One turn of the loop: the writer copies this document and starts.
            context.iteration(false);
            document.apply("level", level(2.0), |_| {});

            wait(|| !document.inner.saving.get());

            let saved = BigVersionedJsonStore::<TestSettings>::new(&path, 1)
                .read()
                .unwrap();
            assert_eq!(saved, Some(level(2.0)));
            let _ = fs::remove_file(&path);
        }

        #[test]
        fn change_stream_follows_live_document() {
            let path = settings_test_path("change-stream");
            let document = BigSettingsDocument::load_or_default(&path, 2).unwrap();
            let calls = Rc::new(Cell::new(0));
            let stream = document.change_stream();
            let _token = stream.subscribe({
                let calls = Rc::clone(&calls);
                move |key| {
                    assert_eq!(key, "enabled");
                    calls.set(calls.get() + 1);
                }
            });

            document.apply_change(
                "enabled",
                |settings: &mut TestSettings| settings.enabled = true,
                |_| {},
            );

            assert_eq!(
                calls.get(),
                1,
                "change_stream() must return the stream used by document changes"
            );
            wait(|| !document.inner.saving.get());
            let _ = fs::remove_file(path);
        }

        #[test]
        fn document_round_trips_through_store() {
            let path = settings_test_path("round-trip");
            let document = BigSettingsDocument::load_or_default(&path, 2).unwrap();

            document.apply_change(
                "enabled",
                |settings: &mut TestSettings| {
                    settings.enabled = true;
                    settings.level = 7.0;
                },
                |error| panic!("{error}"),
            );
            wait(|| !document.inner.saving.get());

            let reloaded = BigSettingsDocument::<TestSettings>::load_or_default(&path, 2)
                .unwrap()
                .snapshot();
            assert_eq!(
                reloaded,
                TestSettings {
                    enabled: true,
                    level: 7.0,
                }
            );
            let _ = fs::remove_file(path);
        }

        #[test]
        fn newer_document_version_is_refused() {
            let path = settings_test_path("version-newer");
            let envelope = json!({
                "version": 3,
                "data": { "active": true }
            });
            let bytes = serde_json::to_vec_pretty(&envelope).unwrap();
            fs::write(&path, &bytes).unwrap();

            assert!(matches!(
                BigSettingsDocument::<TestSettings>::load_or_default(&path, 2),
                Err(BigStorageError::UnknownVersion {
                    found: 3,
                    supported_up_to: 2
                })
            ));
            // Left untouched: this build cannot promise to keep fields it does
            // not know, so it must not replace the file either.
            assert_eq!(fs::read(&path).unwrap(), bytes);
            let _ = fs::remove_file(path);
        }

        /// An unreadable file is never replaced by the defaults standing in for it.
        #[test]
        fn unreadable_file_is_kept() {
            for bytes in [
                serde_json::to_vec(&json!({ "version": 3, "data": { "active": true } })).unwrap(),
                b"{ not json".to_vec(),
            ] {
                let path = settings_test_path("unreadable");
                fs::write(&path, &bytes).unwrap();
                let document =
                    BigSettingsDocument::<TestSettings>::open(BigVersionedJsonStore::new(&path, 2));

                document.apply_change(
                    "enabled",
                    |settings| settings.enabled = true,
                    |error| panic!("{error}"),
                );

                assert!(document.snapshot().enabled);
                assert!(!document.inner.saving.get());
                assert_eq!(fs::read(&path).unwrap(), bytes);
                let _ = fs::remove_file(path);
            }
        }

        /// A document from an older version loads instead of being called corrupt.
        ///
        /// Comparing the version for equality would report the person's settings
        /// as broken after running a newer build once and then the packaged one.
        #[test]
        fn older_document_version_loads() {
            let path = settings_test_path("version-older");
            fs::write(
                &path,
                serde_json::to_vec_pretty(&json!({
                    "version": 1,
                    "data": { "enabled": true, "level": 0.5 }
                }))
                .unwrap(),
            )
            .unwrap();

            let document = BigSettingsDocument::<TestSettings>::load_or_default(&path, 2)
                .expect("an older document loads");
            assert!(document.snapshot().enabled);
            let _ = fs::remove_file(path);
        }
    }
}
