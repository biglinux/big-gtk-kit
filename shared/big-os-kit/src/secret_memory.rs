// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Overwriting memory that held a password before it is let go.

/// Zero `bytes` in a way the optimizer cannot drop as a dead store, so a
/// password does not outlive its use in freed heap memory.
#[expect(clippy::disallowed_methods, reason = "the owner of the ban")]
pub fn wipe(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        // SAFETY: `byte` is a valid, aligned, exclusive reference.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// [`wipe`] for text: the bytes of `text` are zeroed in place and it is left
/// empty.
pub fn wipe_text(text: &mut String) {
    let mut bytes = std::mem::take(text).into_bytes();
    wipe(&mut bytes);
}

#[cfg(test)]
mod tests {
    #[test]
    fn wiped_text_is_overwritten() {
        let mut bytes = "s3cret ç".to_owned().into_bytes();
        super::wipe(&mut bytes);
        assert_eq!(bytes.len(), "s3cret ç".len());
        assert!(bytes.iter().all(|byte| *byte == 0));
    }
}
