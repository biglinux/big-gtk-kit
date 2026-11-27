// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! CRC-32 with the reflected IEEE polynomial `0xEDB88320`, the one PNG
//! chunks, gzip and zlib carry.

/// The CRC of each byte value, made at compile time.
const TABLE: [u32; 256] = {
    let mut table = [0_u32; 256];
    let mut value = 0_u32;
    while value < 256 {
        let mut crc = value;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
            bit += 1;
        }
        table[value as usize] = crc;
        value += 1;
    }
    table
};

/// The CRC-32 of `bytes`, as a PNG chunk or a gzip trailer holds it.
#[must_use]
pub fn of(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(!0_u32, |crc, &byte| {
        TABLE[((crc ^ u32::from(byte)) & 0xFF) as usize] ^ (crc >> 8)
    })
}

#[cfg(test)]
mod tests {
    use super::of;

    /// The check value of the CRC catalogue for CRC-32/ISO-HDLC, and the
    /// chunk every PNG ends with (its type, `IEND`, and the CRC after it).
    #[test]
    fn known_answers() {
        assert_eq!(of(b""), 0);
        assert_eq!(of(b"123456789"), 0xCBF4_3926);
        assert_eq!(of(b"IEND"), 0xAE42_6082);
    }
}
