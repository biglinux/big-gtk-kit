// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! GTK-free OS primitives for the BigLinux programs: atomic and versioned JSON
//! [`storage`], directory metadata scans, process
//! descriptors ([`pidfd`]), whether a typed name can be a file's ([`file_name`]),
//! checksums ([`crc32`]), signals read from a pipe ([`signal_pipe`]), a first
//! capital letter ([`first_capital`]), `file://` URIs read as path bytes
//! ([`uri_path`]), bytes handed over in an anonymous file (`memfd`), calendar
//! days (`civil_date`), unguessable ids ([`random`]), a hash that never
//! changes between Rust releases ([`stable_hash`]), and the in-memory log ring.

pub mod bench;
pub mod caller;
/// Days of the calendar, counted from 1 January 1970.
pub mod civil_date;
pub mod compositor_session;
pub mod confinement;
pub mod crash_reports;
pub mod crc32;
pub mod directory_listing;
pub mod exit_wait;
pub mod file_io;
pub mod file_kinds;
pub mod file_name;
pub mod filesystem_capacity;
pub mod first_capital;
pub mod git;
#[cfg(feature = "http-client")]
pub mod http_client;
#[cfg(feature = "kdeconnect")]
pub mod kdeconnect;
#[cfg(feature = "keyring")]
pub mod keyring;
#[cfg(feature = "memfd")]
pub mod memfd;
pub mod microdaemon;
pub mod mime_comment;
pub mod mount_table;
pub mod pidfd;
pub mod pressure_protocol;
mod process_locale;
pub mod random;
pub mod resource_budget;
#[cfg(feature = "seat")]
pub mod seat;
pub mod secret_memory;
pub mod serde_path;
pub mod service_ready;
pub mod session_resources;
pub mod settings;
pub mod shell_word;
pub mod signal_pipe;
pub mod stable_hash;
pub mod storage;
pub mod subprocess;
pub mod system_memory;
pub mod task;
#[cfg(feature = "test-bus")]
pub mod test_bus;
#[cfg(feature = "text-fold")]
pub mod text_fold;
pub mod uri_path;
pub mod users;
pub mod usershare;

pub use process_locale::{decimal_mark, init_c_numeric_locale};
