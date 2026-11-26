// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The mount table, read once and again only after a mount changes.
//!
//! The kernel raises `POLLPRI` on an open `/proc/self/mountinfo` when the table
//! changes, so asking whether it changed is one `poll(2)` on a descriptor kept
//! open, with no thread and no timer.

use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Seek};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rustix::event::{PollFd, PollFlags, Timespec, poll};

struct Watched {
    file: File,
    table: Arc<str>,
}

static TABLE: Mutex<Option<Watched>> = Mutex::new(None);

/// The text of `/proc/self/mountinfo`. The same `Arc` comes back until a mount
/// changes, so a caller can keep what it parsed from it and compare with
/// [`Arc::ptr_eq`]. Empty when the table cannot be read.
#[must_use]
pub fn mountinfo() -> Arc<str> {
    let mut slot = TABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(watched) = slot.as_mut() {
        if !changed(&watched.file) {
            return Arc::clone(&watched.table);
        }
        match read(&mut watched.file) {
            Some(table) => {
                watched.table = table;
                return Arc::clone(&watched.table);
            }
            None => *slot = None,
        }
    }
    let Ok(mut file) = File::open("/proc/self/mountinfo") else {
        return Arc::from("");
    };
    let Some(table) = read(&mut file) else {
        return Arc::from("");
    };
    let watched = slot.insert(Watched { file, table });
    Arc::clone(&watched.table)
}

/// Whether the kernel flagged the table changed since `file` last read it; a
/// failed look counts as a change, so the table is read again.
fn changed(file: &File) -> bool {
    let mut request = [PollFd::new(file, PollFlags::PRI)];
    !matches!(poll(&mut request, Some(&Timespec::default())), Ok(0))
}

/// One mount listed in `/proc/self/mountinfo`, its paths with the kernel's
/// octal escapes undone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount<'a> {
    /// `major:minor` of the filesystem, the same for every mount of it.
    pub device: &'a str,
    /// The folder of the filesystem mounted here: `/` for a whole disk,
    /// `/@home` for a Btrfs subvolume.
    pub root: PathBuf,
    /// Where it is mounted.
    pub point: PathBuf,
    /// The filesystem type: `ext4`, `nfs4`, `fuse.sshfs`.
    pub filesystem: &'a str,
    /// What was mounted: a device node, a server's share or a name.
    pub source: String,
}

/// The mounts in `table`, the text of `/proc/self/mountinfo`. A line that is
/// not a mount is skipped.
pub fn mounts(table: &str) -> impl Iterator<Item = Mount<'_>> {
    table.lines().filter_map(parse_line)
}

/// A line as `proc_pid_mountinfo(5)` lays it out: the mount point is the fifth
/// field, and the filesystem type and source follow the ` - ` that ends the
/// optional fields.
fn parse_line(line: &str) -> Option<Mount<'_>> {
    let (mount, rest) = line.split_once(" - ")?;
    let mut fields = mount.split(' ').skip(2);
    let device = fields.next()?;
    let root = OsString::from_vec(unescape(fields.next()?.as_bytes())).into();
    let point = OsString::from_vec(unescape(fields.next()?.as_bytes())).into();
    let mut rest = rest.split(' ');
    let filesystem = rest.next()?;
    let source = String::from_utf8_lossy(&unescape(rest.next()?.as_bytes())).into_owned();
    Some(Mount {
        device,
        root,
        point,
        filesystem,
        source,
    })
}

/// A mountinfo field with the escapes put back: the kernel writes a space, a
/// tab, a newline and a backslash as `\040`, `\011`, `\012` and `\134`.
pub(crate) fn unescape(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(field.len());
    let mut index = 0;
    while index < field.len() {
        if field[index] == b'\\'
            && let Some(code) = field
                .get(index + 1..index + 4)
                .and_then(|digits| std::str::from_utf8(digits).ok())
                .and_then(|digits| u8::from_str_radix(digits, 8).ok())
        {
            out.push(code);
            index += 4;
        } else {
            out.push(field[index]);
            index += 1;
        }
    }
    out
}

/// The mount in `table` (the text of `/proc/self/mountinfo`) that holds
/// `path`: the longest mount point that contains it.
#[must_use]
pub fn mount_of<'a>(table: &'a str, path: &Path) -> Option<Mount<'a>> {
    mounts(table)
        .filter(|mount| path.starts_with(&mount.point))
        .max_by_key(|mount| mount.point.as_os_str().len())
}

/// Whether `folder` is on a mount of `table` that is not a network share: a
/// process id found there is this machine's. A folder the table holds no mount
/// for, as when it could not be read, is not.
pub(crate) fn is_local(table: &str, folder: &Path) -> bool {
    std::fs::canonicalize(folder).is_ok_and(|folder| {
        mount_holding(table, &folder).is_some_and(|(_, filesystem)| !is_network(&filesystem))
    })
}

/// The source and the filesystem type of the mount [`mount_of`] finds.
#[must_use]
pub fn mount_holding(table: &str, path: &Path) -> Option<(String, String)> {
    mount_of(table, path).map(|mount| (mount.source, mount.filesystem.to_owned()))
}

/// The filesystem type of the FUSE mount GVfs gives its locations.
pub const GVFS_FUSE: &str = "fuse.gvfsd-fuse";

/// Whether `path` is inside the FUSE mount GVfs gives its locations (an FTP
/// server, a phone, a share opened in a file manager). `table` is the text of
/// `/proc/self/mountinfo`.
#[must_use]
pub fn is_gvfs(table: &str, path: &Path) -> bool {
    mount_holding(table, path).is_some_and(|(_, filesystem)| filesystem == GVFS_FUSE)
}

/// Whether a filesystem of type `filesystem` is reached over the network: an
/// NFS or SMB share, an SSH or cloud mount, or one of GVfs's.
#[must_use]
pub fn is_network(filesystem: &str) -> bool {
    const NETWORK: &[&str] = &[
        "nfs",
        "nfs4",
        "cifs",
        "smb3",
        "smbfs",
        "9p",
        "afs",
        "ceph",
        "glusterfs",
        "davfs",
        "fuse.sshfs",
        "fuse.kio-fuse",
        GVFS_FUSE,
        "fuse.rclone",
        "fuse.s3fs",
        "fuse.davfs2",
        "fuse.curlftpfs",
    ];
    NETWORK.contains(&filesystem)
}

/// Whether a look into a filesystem of type `filesystem` can block for as long
/// as a server takes to answer: a network share, or anything behind FUSE (a
/// phone, an archive, a cloud drive), which answers only when its process does.
#[must_use]
pub fn may_stall(filesystem: &str) -> bool {
    is_network(filesystem) || filesystem.starts_with("fuse")
}

/// Whether the mount source `source` (`/dev/mapper/luks-1`) is a dm-crypt
/// volume, directly or through the layers stacked on one (LVM on LUKS).
#[must_use]
pub fn on_dm_crypt(source: &str) -> bool {
    std::fs::canonicalize(source)
        .ok()
        .and_then(|device| {
            device
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .is_some_and(|name| dm_crypt_below(&name, 0))
}

/// Whether the block device `name` (`dm-3`) is a dm-crypt target, or is built
/// on one, looking at most eight layers down.
fn dm_crypt_below(name: &str, depth: u8) -> bool {
    let device = Path::new("/sys/class/block").join(name);
    if std::fs::read_to_string(device.join("dm/uuid")).is_ok_and(|uuid| uuid.starts_with("CRYPT-"))
    {
        return true;
    }
    depth < 8
        && std::fs::read_dir(device.join("slaves")).is_ok_and(|slaves| {
            slaves
                .flatten()
                .any(|slave| dm_crypt_below(&slave.file_name().to_string_lossy(), depth + 1))
        })
}

/// The kernel escapes only space, tab, newline and backslash, so a mount point
/// may hold any other byte: one that is not UTF-8 spoils that line alone.
fn read(file: &mut File) -> Option<Arc<str>> {
    file.rewind().ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(Arc::from(String::from_utf8_lossy(&bytes)))
}

#[cfg(test)]
mod tests {
    /// A mount point that is not UTF-8 leaves the other mounts readable.
    #[test]
    fn foreign_bytes_spoil_one_line() {
        let path = std::env::temp_dir().join(format!("mountinfo-{}", std::process::id()));
        std::fs::write(
            &path,
            b"22 1 0:21 / / rw - btrfs /dev/sda2 rw\n90 22 0:60 / /media/\xff rw - nfs4 server:/x rw\n",
        )
        .unwrap();
        let table = super::read(&mut std::fs::File::open(&path).unwrap()).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            super::mount_holding(&table, std::path::Path::new("/home")),
            Some(("/dev/sda2".to_owned(), "btrfs".to_owned()))
        );
    }

    #[test]
    fn holding_mount_is_the_longest() {
        let table = "\
22 1 0:21 / / rw,relatime shared:1 - btrfs /dev/nvme0n1p2 rw
35 22 0:32 / /run/user/1000 rw,nosuid shared:2 - tmpfs tmpfs rw,mode=700
80 35 0:51 / /run/user/1000/gvfs rw,nosuid,nodev shared:3 - fuse.gvfsd-fuse gvfsd-fuse rw,user_id=1000";
        let filesystem = |path: &str| {
            super::mount_holding(table, std::path::Path::new(path))
                .map(|(_, filesystem)| filesystem)
        };
        assert_eq!(
            filesystem(
                "/run/user/1000/gvfs/ftp:host=192.168.1.27,port=3575,user=pc/device/Download"
            )
            .as_deref(),
            Some("fuse.gvfsd-fuse")
        );
        assert_eq!(
            filesystem("/run/user/1000/gvfs-cache").as_deref(),
            Some("tmpfs")
        );
        assert_eq!(filesystem("/home/pc/Downloads").as_deref(), Some("btrfs"));
    }

    /// The autologin page asks this of `/home`: its own mount when it has one,
    /// else the root's.
    #[test]
    fn home_mount_names_its_device() {
        let split = "22 1 0:21 /@ / rw - btrfs /dev/mapper/luks-1 rw\n\
                     30 22 0:22 /@home /home rw - btrfs /dev/mapper/luks-2 rw\n";
        let joined = "22 1 0:21 /@ / rw - btrfs /dev/mapper/luks-1 rw\n";
        let source = |table| {
            super::mount_holding(table, std::path::Path::new("/home")).map(|(source, _)| source)
        };
        assert_eq!(source(split).as_deref(), Some("/dev/mapper/luks-2"));
        assert_eq!(source(joined).as_deref(), Some("/dev/mapper/luks-1"));
    }

    #[test]
    fn mount_point_with_space_reads_whole() {
        let table = "\
50 29 8:17 / /run/media/me/My\\040Disk rw,nosuid shared:7 master:1 - ext4 /dev/sdb1 rw
not a mount line

51 29 0:60 /a\\134b /mnt/tab\\011x rw - fuse.sshfs me@host:/srv/my\\040files rw";
        let mounts: Vec<_> = super::mounts(table).collect();
        assert_eq!(
            mounts[0],
            super::Mount {
                device: "8:17",
                root: "/".into(),
                point: "/run/media/me/My Disk".into(),
                filesystem: "ext4",
                source: "/dev/sdb1".to_owned(),
            }
        );
        assert_eq!(mounts[1].root, std::path::Path::new("/a\\b"));
        assert_eq!(mounts[1].point, std::path::Path::new("/mnt/tab\tx"));
        assert_eq!(mounts[1].source, "me@host:/srv/my files");
        assert_eq!(mounts.len(), 2);
        assert_eq!(super::mounts("").count(), 0);
    }

    #[test]
    fn remote_filesystems_are_named() {
        for remote in [
            "nfs4",
            "cifs",
            "fuse.sshfs",
            "fuse.kio-fuse",
            "fuse.gvfsd-fuse",
            "fuse.rclone",
        ] {
            assert!(super::is_network(remote), "{remote}");
        }
        for local in ["btrfs", "ext4", "vfat", "fuse.portal", "tmpfs", "overlay"] {
            assert!(!super::is_network(local), "{local}");
        }
        assert!(super::may_stall("fuse.portal") && super::may_stall("nfs"));
        assert!(!super::may_stall("ext4"));
    }

    #[test]
    fn unchanged_table_is_shared() {
        let first = super::mountinfo();
        assert!(!first.is_empty());
        assert!(std::sync::Arc::ptr_eq(&first, &super::mountinfo()));
    }

    /// Run as `unshare -rm <test binary> --ignored`: it mounts in a private
    /// mount namespace, which goes away with the process.
    #[test]
    #[ignore = "machine-changing: needs a private mount namespace"]
    fn mount_change_is_seen() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let before = super::mountinfo();
        let target = std::env::temp_dir().join(format!("mount-table-{}", std::process::id()));
        std::fs::create_dir_all(&target).expect("scratch directory");
        let path = CString::new(target.as_os_str().as_bytes()).expect("no NUL in the path");
        // SAFETY: both strings are NUL-terminated and outlive the call; a null
        // data pointer is valid for tmpfs.
        let mounted = unsafe {
            libc::mount(
                c"scratch".as_ptr(),
                path.as_ptr(),
                c"tmpfs".as_ptr(),
                0,
                std::ptr::null(),
            )
        };
        let after = super::mountinfo();
        assert_eq!(mounted, 0, "{}", std::io::Error::last_os_error());
        assert!(!std::sync::Arc::ptr_eq(&before, &after));
        assert!(after.contains(target.to_str().expect("utf-8 path")));
    }
}
