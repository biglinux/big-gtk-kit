// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Samba user shares, through `net usershare`.
//!
//! Samba's own front door for a person in the `sambashare` group: each share
//! is a small file in the directory `usershare path` names, `smbd` picks a
//! change up without a restart, and nothing needs root or an `smb.conf` edit.
//! Whether this machine lets the person share at all depends on their groups,
//! that directory's mode and the `usershare` settings, and the command is the
//! one thing that weighs all three, so a failed [`shares`] is the answer.
//!
//! Every call runs a process: keep them off a GUI main loop.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::subprocess::{BigSubprocessOutputMode, BigSubprocessSpec};

/// Samba's command, where either kind of distribution puts it.
const NET: [&str; 2] = ["/usr/bin/net", "/usr/sbin/net"];

/// One shared folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Share {
    /// What it is called on the network.
    pub name: String,
    /// The folder shared.
    pub path: PathBuf,
    /// Samba's free-text description; empty when there is none.
    pub comment: String,
    /// Whether the other side may change what is in it.
    pub writable: bool,
    /// Whether guests may open it, or only somebody with an account here.
    pub anyone: bool,
}

/// Why Samba did not do what was asked.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `net` is not on this machine.
    #[error("Samba is not installed")]
    NotInstalled,
    /// `net` could not be started.
    #[error("{0}")]
    Spawn(String),
    /// `net` ran and refused; its own explanation, trimmed.
    #[error("{0}")]
    Refused(String),
}

/// Why a share name cannot be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameRefused {
    /// Nothing but spaces.
    Empty,
    /// A path separator, a leading `-`, or a character `smb.conf` gives its
    /// own meaning to.
    Characters,
    /// Something is already shared under this name, ignoring case.
    Taken,
    /// An account on this computer has this name ([`account_named`]); `net`
    /// refuses it.
    Account,
}

impl NameRefused {
    /// The refusal in words, read through the caller's catalog, which lists
    /// this file in its `POTFILES`.
    #[must_use]
    pub fn words(self, gettext: impl Fn(&str) -> String) -> String {
        match self {
            Self::Empty => gettext("A shared folder needs a name"),
            Self::Characters => {
                gettext("That name cannot be used on a network. Letters, numbers and spaces work.")
            }
            Self::Taken => gettext("Something is already shared under that name"),
            Self::Account => gettext("An account on this computer has that name"),
        }
    }
}

/// What the folder's own mode allows others, against what a share promises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filesystem {
    /// Others can do what the share promises.
    Agrees,
    /// Others cannot list it, so no share of it can be opened.
    Unreadable,
    /// Readable, but the share promises changes the folder will refuse.
    Unwritable,
}

/// Whether Samba's command is installed.
pub fn installed() -> bool {
    command().is_some()
}

fn command() -> Option<&'static str> {
    NET.into_iter().find(|path| Path::new(path).is_file())
}

fn net(arguments: &[&OsStr]) -> Result<String, Error> {
    let program = command().ok_or(Error::NotInstalled)?;
    let output = BigSubprocessSpec::builder()
        .program(program)
        .args(arguments)
        // `net` waits on smbd's lock files; a wedged one must not hold a worker
        // for the default half minute.
        .timeout(std::time::Duration::from_secs(5))
        .stdout(BigSubprocessOutputMode::Capture)
        .stderr(BigSubprocessOutputMode::Capture)
        .allow_list(NET)
        .build()
        .run()
        .map_err(|error| Error::Spawn(error.to_string()))?;
    if output.status.success() {
        Ok(output.stdout_lossy())
    } else {
        Err(Error::Refused(output.stderr_lossy().trim().to_owned()))
    }
}

/// Every folder this person shares; `Err` when Samba will not let them share.
///
/// # Errors
///
/// [`Error::NotInstalled`] without Samba, otherwise why `net` failed.
pub fn shares() -> Result<Vec<Share>, Error> {
    net(&["usershare".as_ref(), "info".as_ref()]).map(|text| parse(&text))
}

/// Share `share.path` as `share.name`; an existing name is replaced, which is
/// how a share is edited.
///
/// # Errors
///
/// As [`shares`].
pub fn add(share: &Share) -> Result<(), Error> {
    // Full control or read: the only two grants written here.
    let grant = if share.writable {
        "Everyone:F"
    } else {
        "Everyone:R"
    };
    let guests = if share.anyone {
        "guest_ok=y"
    } else {
        "guest_ok=n"
    };
    // `--` ends `net`'s options: a name or comment may start with `-`.
    net(&[
        "usershare".as_ref(),
        "add".as_ref(),
        "--".as_ref(),
        share.name.as_ref(),
        share.path.as_os_str(),
        share.comment.as_ref(),
        grant.as_ref(),
        guests.as_ref(),
    ])
    .map(drop)
}

/// Stop sharing `name`.
///
/// # Errors
///
/// As [`shares`].
pub fn remove(name: &str) -> Result<(), Error> {
    net(&[
        "usershare".as_ref(),
        "delete".as_ref(),
        "--".as_ref(),
        name.as_ref(),
    ])
    .map(drop)
}

/// `net usershare info`: one block per share.
///
/// ```text
/// [Public]
/// path=/home/someone/Public
/// comment=
/// usershare_acl=Everyone:R,
/// guest_ok=n
/// ```
fn parse(text: &str) -> Vec<Share> {
    let mut found = Vec::new();
    let mut share: Option<Share> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            found.extend(share.take());
            share = Some(Share {
                name: name.to_owned(),
                path: PathBuf::new(),
                comment: String::new(),
                writable: false,
                anyone: false,
            });
            continue;
        }
        let (Some(here), Some((key, value))) = (share.as_mut(), line.split_once('=')) else {
            continue;
        };
        match key {
            "path" => here.path = PathBuf::from(value),
            "comment" => here.comment = value.to_owned(),
            // Only full control for everyone reads as writable: a list that
            // grants it to one person would come back from `add` granting it
            // to all.
            "usershare_acl" => {
                here.writable = value
                    .split(',')
                    .any(|grant| grant.eq_ignore_ascii_case("Everyone:F"));
            }
            "guest_ok" => here.anyone = value == "y",
            _ => {}
        }
    }
    found.extend(share);
    found
}

/// A name Samba will take next to `others`, from the name alone: whether an
/// account has it is [`account_named`], which reads the account database.
///
/// Refused, never corrected: it is printed on other people's screens.
///
/// # Errors
///
/// Which rule `wanted` breaks.
pub fn check_name(wanted: &str, others: &[Share]) -> Result<(), NameRefused> {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return Err(NameRefused::Empty);
    }
    // Samba's `INVALID_SHARENAME_CHARS`, which `net` refuses in English, and
    // the brackets that would end the name in the share list.
    if wanted.contains([
        '%', '<', '>', '*', '?', '|', '/', '\\', '+', '=', ';', ':', '"', ',', '[', ']',
    ]) || wanted.starts_with('-')
    {
        return Err(NameRefused::Characters);
    }
    // Samba names a share's file by the name in lower case, so a second name
    // that differs only in case, accented letters included, replaces the first.
    let folded = lower_case(wanted);
    if others.iter().any(|share| lower_case(&share.name) == folded) {
        return Err(NameRefused::Taken);
    }
    Ok(())
}

/// Whether an account on this computer has `name`, ignoring case, which `net`
/// refuses as a share name ([`NameRefused::Account`]). Reads the account
/// database, which may ask a directory server: run it on a worker.
#[must_use]
pub fn account_named(name: &str) -> bool {
    crate::users::id_of(&lower_case(name.trim())).is_some()
}

/// Share `share` in place of the share named `old`, which goes unless Samba
/// stored `share` under it: a name that differs only in case replaces the
/// same file, and deleting `old` then would delete `share`.
///
/// # Errors
///
/// As [`shares`]; a failed `add` leaves `old` shared as it was, and a failure
/// after it leaves the folder shared under both names.
pub fn rename(old: &str, share: &Share) -> Result<(), Error> {
    add(share)?;
    if old != share.name && shares()?.iter().any(|listed| listed.name == old) {
        remove(old)
    } else {
        Ok(())
    }
}

/// `name` in lower case one character at a time, with no final sigma: a
/// superset of Samba's table, which folds no character this leaves alone and
/// keeps a few apart (`İ`, `Ș`) that this folds.
fn lower_case(name: &str) -> String {
    name.chars()
        .map(|character| character.to_lowercase().next().unwrap_or(character))
        .collect()
}

const OTHERS_READ: u32 = 0o004;
const OTHERS_WRITE: u32 = 0o002;
const OTHERS_ENTER: u32 = 0o001;

/// Weigh a folder's `mode` against a share that is `writable` or not.
///
/// Samba serves a guest as a user with no special rights, so the "others"
/// bits decide. Listing a directory needs both read and enter.
pub fn filesystem_says(mode: u32, writable: bool) -> Filesystem {
    if mode & OTHERS_READ == 0 || mode & OTHERS_ENTER == 0 {
        Filesystem::Unreadable
    } else if writable && mode & OTHERS_WRITE == 0 {
        Filesystem::Unwritable
    } else {
        Filesystem::Agrees
    }
}

/// The outermost folder above `path` that others cannot enter, which keeps
/// them out of `path` whatever its own mode says. A home folder with mode
/// `0700` is the usual one. Reads the filesystem: run it on a worker.
pub fn enclosing(path: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    path.ancestors()
        .skip(1)
        .filter(|above| {
            std::fs::metadata(above)
                .is_ok_and(|found| found.permissions().mode() & OTHERS_ENTER == 0)
        })
        .last()
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `net usershare info` printed on a real machine.
    const REAL: &str = "[bigshellprobe]\n\
                        path=/home/user/.cache/ushare-probe\n\
                        comment=probe\n\
                        usershare_acl=Everyone:R,\n\
                        guest_ok=n\n";

    #[test]
    fn the_commands_own_output_reads_back() {
        assert_eq!(
            parse(REAL),
            vec![Share {
                name: String::from("bigshellprobe"),
                path: PathBuf::from("/home/user/.cache/ushare-probe"),
                comment: String::from("probe"),
                writable: false,
                anyone: false,
            }]
        );
    }

    /// `add` writes one grant for everyone, so full control for one person
    /// must not read as full control for all.
    #[test]
    fn one_persons_full_control_reads_back_read_only() {
        let acl = |grants: &str| {
            parse(&format!("[Docs]\npath=/srv/docs\nusershare_acl={grants}\n"))[0].writable
        };
        assert!(!acl("alice:F,Everyone:R,"));
        assert!(!acl("BUILTIN\\Users:F,"));
        assert!(acl("Everyone:F,"));
        assert!(acl("alice:R,everyone:f,"));
    }

    /// A parser that only pushes on the next header drops the last share.
    #[test]
    fn several_shares_keep_the_last() {
        let found = parse(&format!(
            "{REAL}[Family]\npath=/srv/family\ncomment=\nusershare_acl=Everyone:F,\nguest_ok=y\n"
        ));
        assert_eq!(found.len(), 2);
        assert!(found[1].writable && found[1].anyone);
        assert_eq!(found[1].path, PathBuf::from("/srv/family"));
        assert!(parse("").is_empty());
    }

    #[test]
    fn names_are_refused_with_a_reason() {
        let others = parse("[Public]\npath=/srv/public\n");
        assert_eq!(check_name("", &others), Err(NameRefused::Empty));
        assert_eq!(check_name("   ", &others), Err(NameRefused::Empty));
        assert_eq!(
            check_name("home/mine", &others),
            Err(NameRefused::Characters)
        );
        assert_eq!(check_name("a\\b", &others), Err(NameRefused::Characters));
        assert_eq!(check_name("-hidden", &others), Err(NameRefused::Characters));
        // What `net` refuses, and the share list's brackets.
        for name in ["Fotos+Videos", "a=b", "a;b", "a,b", "[a]"] {
            assert_eq!(
                check_name(name, &others),
                Err(NameRefused::Characters),
                "{name}"
            );
        }
        assert_eq!(check_name("public", &others), Err(NameRefused::Taken));
        assert_eq!(check_name("Family photos", &others), Ok(()));
        let accented = parse("[Músicas]\npath=/srv/music\n");
        assert_eq!(check_name("MÚSICAS", &accented), Err(NameRefused::Taken));
        assert_eq!(check_name("Musicas", &accented), Ok(()));
    }

    #[test]
    fn account_names_are_found() {
        assert!(account_named("ROOT"));
        assert!(!account_named("Family photos"));
    }

    #[test]
    fn folder_mode_limits_the_share() {
        assert_eq!(filesystem_says(0o700, false), Filesystem::Unreadable);
        assert_eq!(filesystem_says(0o744, false), Filesystem::Unreadable);
        assert_eq!(filesystem_says(0o755, false), Filesystem::Agrees);
        assert_eq!(filesystem_says(0o755, true), Filesystem::Unwritable);
        assert_eq!(filesystem_says(0o757, true), Filesystem::Agrees);
    }

    #[test]
    fn closed_parent_keeps_others_out() {
        use std::os::unix::fs::PermissionsExt;
        let outer = std::env::temp_dir().join(format!("usershare-{}", std::process::id()));
        let inner = outer.join("shared");
        std::fs::create_dir_all(&inner).unwrap();
        let chmod = |path: &Path, mode| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        // The temporary directory may itself sit under a closed folder.
        let above = enclosing(&outer);
        chmod(&outer, 0o755);
        let open = enclosing(&inner);
        chmod(&outer, 0o700);
        let closed = enclosing(&inner);
        std::fs::remove_dir_all(&outer).unwrap();
        assert_eq!(open, above);
        assert_eq!(closed, above.or(Some(outer)));
    }
}
