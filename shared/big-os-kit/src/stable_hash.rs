// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! A 64-bit hash that stays the same across Rust releases and machines, for
//! names written to disk and colors a person comes to recognize.
//! `std::hash::DefaultHasher` may change its algorithm in any release.

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a over `parts` read one after another. Parts are not delimited, so
/// only the last one may vary in length where two inputs must stay apart.
#[must_use]
pub fn hash(parts: &[&[u8]]) -> u64 {
    let mut state = FNV_OFFSET_BASIS;
    for byte in parts.iter().flat_map(|part| part.iter()) {
        state ^= u64::from(*byte);
        state = state.wrapping_mul(FNV_PRIME);
    }
    state
}

#[cfg(test)]
mod tests {
    use super::hash;

    /// Known answers from the FNV reference test suite (draft-eastlake-fnv).
    #[test]
    fn fnv1a_reference_vectors() {
        assert_eq!(hash(&[]), 0xcbf2_9ce4_8422_2325);
        assert_eq!(hash(&[b"a"]), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(hash(&[b"foobar"]), 0x8594_4171_f739_67e8);
        assert_eq!(hash(&[b"foo", b"bar"]), hash(&[b"foobar"]));
    }
}
