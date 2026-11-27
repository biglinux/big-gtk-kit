// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The system's user database, through NSS: `/etc/passwd`, and also
//! systemd-homed or a network directory when the machine uses one. A lookup
//! may wait on the network, so it belongs on a worker, and a caller that asks
//! often caches the answer.

use std::ffi::{CStr, CString, c_char, c_int};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

/// What a lookup keeps of one entry.
struct Entry {
    /// Nothing when the name is not UTF-8.
    name: Option<String>,
    uid: u32,
    home: PathBuf,
}

/// The user this process runs as.
#[must_use]
pub fn own_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// The groups this process belongs to: its effective group and the
/// supplementary ones, each once.
#[must_use]
pub fn own_group_ids() -> Vec<u32> {
    // getgroups fails only when the list grew between its count and its read;
    // the effective group is then the one known.
    let mut groups: Vec<u32> = rustix::process::getgroups()
        .unwrap_or_default()
        .into_iter()
        .map(rustix::process::Gid::as_raw)
        .collect();
    groups.push(rustix::process::getegid().as_raw());
    groups.sort_unstable();
    groups.dedup();
    groups
}

/// The login name of `uid`, or nothing when no entry has that id or its name
/// is not UTF-8.
#[must_use]
pub fn name_of(uid: u32) -> Option<String> {
    // SAFETY: `lookup` passes an entry, a buffer of the length given and a
    // result pointer, all alive for the call.
    lookup(|entry, buffer, length, found| unsafe {
        libc::getpwuid_r(uid, entry, buffer, length, found)
    })
    .and_then(|entry| entry.name)
}

/// The user id of the login `name`.
#[must_use]
pub fn id_of(name: &str) -> Option<u32> {
    let name = CString::new(name).ok()?;
    // SAFETY: as in `name_of`; `name` is NUL-terminated and outlives the call.
    lookup(|entry, buffer, length, found| unsafe {
        libc::getpwnam_r(name.as_ptr(), entry, buffer, length, found)
    })
    .map(|entry| entry.uid)
}

/// The home folder the database names for `uid`.
#[must_use]
pub fn home_of(uid: u32) -> Option<PathBuf> {
    // SAFETY: as in `name_of`.
    lookup(|entry, buffer, length, found| unsafe {
        libc::getpwuid_r(uid, entry, buffer, length, found)
    })
    .map(|entry| entry.home)
}

/// The account `login`'s picture: AccountsService's `IconFile` when that
/// file is there, then the copy AccountsService keeps itself, then `home`'s
/// `.face.icon` and `.face`. Blocking: it asks the filesystem.
///
/// A record can name a file that is gone, such as `~/.face` deleted while the
/// picture still sits in `/var/lib/AccountsService/icons`. `home` is `None`
/// for another person's account, whose home is not looked in.
#[must_use]
pub fn picture_file(icon_file: &str, login: &str, home: Option<&Path>) -> Option<PathBuf> {
    let mut candidates = vec![
        PathBuf::from(icon_file),
        Path::new("/var/lib/AccountsService/icons").join(login),
    ];
    if let Some(home) = home {
        candidates.extend([home.join(".face.icon"), home.join(".face")]);
    }
    candidates
        .into_iter()
        .find(|path| !path.as_os_str().is_empty() && path.is_file())
}

/// Run one `getpw*_r` call, growing its buffer while the entry does not fit.
fn lookup(
    call: impl Fn(*mut libc::passwd, *mut c_char, usize, *mut *mut libc::passwd) -> c_int,
) -> Option<Entry> {
    // An entry larger than this is not a person's account.
    const MAX_BUFFER: usize = 1 << 20;
    let mut length = 4096;
    loop {
        let mut buffer = vec![0 as c_char; length];
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        let status = call(
            entry.as_mut_ptr(),
            buffer.as_mut_ptr(),
            buffer.len(),
            &raw mut found,
        );
        if status == libc::ERANGE && length < MAX_BUFFER {
            length *= 4;
            continue;
        }
        if status != 0 || found.is_null() {
            return None;
        }
        // SAFETY: on success `found` points at `entry`, filled, and its strings
        // are NUL-terminated inside `buffer`, which is alive here.
        let entry = unsafe { &*found };
        if entry.pw_name.is_null() || entry.pw_dir.is_null() {
            return None;
        }
        // SAFETY: both pointers were checked non-null and point into `buffer`.
        let (name, home) = unsafe { (CStr::from_ptr(entry.pw_name), CStr::from_ptr(entry.pw_dir)) };
        return Some(Entry {
            name: name.to_str().ok().map(str::to_owned),
            uid: entry.pw_uid,
            home: PathBuf::from(std::ffi::OsStr::from_bytes(home.to_bytes())),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record naming a file that is gone falls through to the home folder's
    /// picture, and another person's home is never looked in.
    #[test]
    fn picture_skips_missing_record() {
        let home = std::env::temp_dir().join(format!("big-users-face-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(".face"), b"x").unwrap();
        let gone = home.join("gone.png");
        let login = "no-such-login-for-a-picture";

        assert_eq!(
            picture_file(&gone.to_string_lossy(), login, Some(&home)),
            Some(home.join(".face"))
        );
        assert_eq!(picture_file("", login, None), None);
        assert_eq!(picture_file(&gone.to_string_lossy(), login, None), None);
        std::fs::write(home.join(".face.icon"), b"x").unwrap();
        assert_eq!(
            picture_file("", login, Some(&home)),
            Some(home.join(".face.icon"))
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    /// The process's own group is among the groups it may give an item to,
    /// once.
    #[test]
    fn own_groups_hold_the_effective_one() {
        let groups = own_group_ids();
        let effective = rustix::process::getegid().as_raw();
        assert_eq!(groups.iter().filter(|id| **id == effective).count(), 1);
    }

    #[test]
    fn root_resolves_both_ways() {
        assert_eq!(name_of(0).as_deref(), Some("root"));
        assert_eq!(id_of("root"), Some(0));
    }

    #[test]
    fn unknown_names_resolve_to_nothing() {
        assert_eq!(id_of("no such user here"), None);
        assert_eq!(id_of("nul\0inside"), None);
    }
}
