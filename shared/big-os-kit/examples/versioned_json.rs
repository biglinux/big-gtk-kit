// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Atomically store and reload one versioned JSON document.
//!
//! Run with:
//! `cargo run -p big-os-kit --example versioned_json`

use big_os_kit::storage::BigVersionedJsonStore;
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Preferences {
    theme: String,
    volume: u8,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!("big-os-kit-example-{}.json", std::process::id()));
    let store = BigVersionedJsonStore::new(&path, 1);
    let expected = Preferences {
        theme: "system".into(),
        volume: 75,
    };

    store.write(&expected)?;
    let loaded = store
        .read()?
        .ok_or_else(|| std::io::Error::other("versioned document was not written"))?;
    println!("{}: {loaded:?}", store.path().display());

    std::fs::remove_file(path)?;
    assert_eq!(loaded, expected);
    Ok(())
}
