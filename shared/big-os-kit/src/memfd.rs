// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Bytes handed to another process through an anonymous file's descriptor.

use std::fs::File;
use std::io::{self, Seek, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::ptr::NonNull;

use rustix::fs::{MemfdFlags, SealFlags};
use rustix::mm::{MapFlags, ProtFlags};

/// `parts`, one after the other, in a new anonymous file named `name`, read
/// from its start. A `sealed` file cannot change size or content, nor
/// take more seals, so a reader that maps it may trust what it maps.
///
/// # Errors
///
/// Returns the error Linux gives when the file cannot be made, written,
/// rewound or sealed.
pub fn filled(name: &str, parts: &[&[u8]], sealed: bool) -> io::Result<OwnedFd> {
    let flags = if sealed {
        MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING
    } else {
        MemfdFlags::CLOEXEC
    };
    let mut file = File::from(rustix::fs::memfd_create(name, flags)?);
    for part in parts {
        file.write_all(part)?;
    }
    file.rewind()?;
    if sealed {
        rustix::fs::fcntl_add_seals(
            &file,
            SealFlags::SEAL | SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE,
        )?;
    }
    Ok(file.into())
}

/// A new anonymous file of `len` bytes, mapped read and write and shared:
/// what this process draws into, or reads back, while another process holds
/// the same file by [`Self::fd`] (a `wl_shm` pool, a PipeWire buffer). The
/// file is sealed at that size, so the other holder cannot shrink it under
/// this mapping, which would end this process with `SIGBUS`.
pub struct SharedMapping {
    fd: OwnedFd,
    start: NonNull<u8>,
    len: usize,
}

impl SharedMapping {
    /// Make the file, named `name`, and map all of it.
    ///
    /// # Errors
    ///
    /// The error Linux gives when the file cannot be made or sized, or not
    /// mapped (`len` 0 among the causes).
    pub fn new(name: &str, len: usize) -> io::Result<Self> {
        let fd = rustix::fs::memfd_create(name, MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)?;
        rustix::fs::ftruncate(&fd, u64::try_from(len).map_err(io::Error::other)?)?;
        rustix::fs::fcntl_add_seals(&fd, SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL)?;
        // SAFETY: a new mapping of a file only this value holds yet, at an
        // address the kernel chooses; `Drop` unmaps exactly `len` bytes of it.
        let start = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                len,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                &fd,
                0,
            )?
        };
        let start = NonNull::new(start.cast::<u8>())
            .ok_or_else(|| io::Error::other("the mapping came back at address 0"))?;
        Ok(Self { fd, start, len })
    }

    /// The file, for the other process to map.
    #[must_use]
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// The mapping's length in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the mapping holds no bytes; [`Self::new`] refuses that.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The file's bytes. The other holder of the file may be writing them:
    /// read once its protocol says it is done (a `ready` event).
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: `start` maps `len` readable bytes until `Drop`, which the
        // borrow of `self` outlasts.
        unsafe { std::slice::from_raw_parts(self.start.as_ptr(), self.len) }
    }

    /// The bytes to write, while the other process does not read them (a
    /// `wl_buffer` not yet attached, or released).
    #[must_use]
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: `start` maps `len` writable bytes until `Drop`, and the
        // mutable borrow of `self` is the only one into them here.
        unsafe { std::slice::from_raw_parts_mut(self.start.as_ptr(), self.len) }
    }
}

impl Drop for SharedMapping {
    fn drop(&mut self) {
        // SAFETY: `start` and `len` are the mapping `new` made, unmapped once.
        let _ = unsafe { rustix::mm::munmap(self.start.as_ptr().cast(), self.len) };
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    #[test]
    fn sealed_file_keeps_its_bytes() {
        let mut file = File::from(filled("test", &[b"secret", b"\n"], true).unwrap());
        let mut read = String::new();
        file.read_to_string(&mut read).unwrap();
        assert_eq!(read, "secret\n");
        assert!(file.write_all(b"x").is_err(), "a sealed file took a write");
        assert!(file.set_len(1).is_err(), "a sealed file shrank");
    }

    #[test]
    fn unsealed_file_stays_writable() {
        let mut file = File::from(filled("test", &[b"map", b"\0"], false).unwrap());
        let mut read = Vec::new();
        file.read_to_end(&mut read).unwrap();
        assert_eq!(read, b"map\0");
        file.write_all(b"more").unwrap();
    }

    /// What is written through the mapping is what the file holds, read by
    /// its descriptor as another process would; nothing maps zero bytes.
    #[test]
    fn mapping_shares_its_file() {
        let mut mapping = SharedMapping::new("test", 4096).unwrap();
        assert_eq!(mapping.len(), 4096);
        mapping.bytes_mut()[..5].copy_from_slice(b"frame");
        let mut read = [0_u8; 5];
        rustix::io::pread(mapping.fd(), &mut read, 0).unwrap();
        assert_eq!(&read, b"frame");
        rustix::io::pwrite(mapping.fd(), b"ready", 4091).unwrap();
        assert_eq!(&mapping.bytes()[4091..], b"ready");
        for len in [0, 4095, 4097] {
            assert!(
                rustix::fs::ftruncate(mapping.fd(), len).is_err(),
                "the other holder resized it to {len}"
            );
        }
        assert!(SharedMapping::new("test", 0).is_err());
    }
}
