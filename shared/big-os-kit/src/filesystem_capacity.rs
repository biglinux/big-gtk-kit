// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Filesystem free/total capacity for the mount that contains a path.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Available and total bytes and inodes of the filesystem that holds a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilesystemCapacity {
    /// Bytes available to an unprivileged process (`f_bavail * f_frsize`).
    pub available_bytes: u64,
    /// Total bytes of the filesystem (`f_blocks * f_frsize`).
    pub total_bytes: u64,
    /// Inodes available to an unprivileged process (`f_favail`).
    pub available_inodes: u64,
    /// Total inodes (`f_files`); 0 where inodes are allocated on demand, as
    /// on btrfs.
    pub total_inodes: u64,
}

/// Query the filesystem capacity for `path` through `statvfs(2)`.
///
/// The path is used verbatim; callers pass an existing directory. This does no
/// subprocess work, but on a network mount `statvfs` can block, so callers that
/// must not stall the UI thread should run it off the main loop.
///
/// # Errors
///
/// Returns the underlying `statvfs` error, or `InvalidInput` when the path
/// contains an interior NUL byte.
pub fn filesystem_capacity(path: &Path) -> io::Result<FilesystemCapacity> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let stats = rustix::fs::statvfs(&c_path)?;
    // A filesystem that gives no fragment size counts in blocks.
    let frsize = if stats.f_frsize == 0 {
        stats.f_bsize
    } else {
        stats.f_frsize
    };
    Ok(FilesystemCapacity {
        available_bytes: stats.f_bavail.saturating_mul(frsize),
        total_bytes: stats.f_blocks.saturating_mul(frsize),
        available_inodes: stats.f_favail,
        total_inodes: stats.f_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_filesystem_reports_sane_capacity() {
        let capacity = filesystem_capacity(Path::new("/")).unwrap();
        assert!(capacity.total_bytes > 0);
        assert!(capacity.available_bytes <= capacity.total_bytes);
    }

    #[test]
    fn interior_nul_path_is_refused() {
        use std::os::unix::ffi::OsStrExt;
        let path = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/a\0b"));
        assert_eq!(
            filesystem_capacity(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
