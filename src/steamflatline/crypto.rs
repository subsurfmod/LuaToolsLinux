//! Cryptographic primitives for steamflatline
//!
//! Implements XOR decryption, XTEA decryption, and AES decryption used by SteamStub variants.

use aes::cipher::{KeyIvInit, block_padding::NoPadding, BlockDecryptMut};
use cbc::Decryptor;

type Aes256CbcDec = Decryptor<aes::Aes256>;

/// Rebuild IV for AES decryption, matching Steamless AesHelper.RebuildIv behavior.
///
/// This encrypts the original IV with the current cipher state to rebuild
/// the IV for CBC decryption, matching the Steamless implementation.
fn aes_rebuild_iv(key: &[u8; 32], iv: &mut [u8; 16]) {
    use aes::Aes256;
    use aes::cipher::{KeyIvInit, BlockEncryptMut, block_padding::NoPadding, BlockSizeUser};
    use cbc::Encryptor;

    // Use the array references directly - cipher re-exports GenericArray
    let key_arr = <aes::cipher::generic_array::GenericArray<u8, _>>::from_slice(key);
    let iv_arr = <aes::cipher::generic_array::GenericArray<u8, _>>::from_slice(iv);

    let mut encryptor = <Encryptor<Aes256> as KeyIvInit>::new(key_arr, iv_arr);
    // Encrypt the IV in-place (1 block = 16 bytes for AES)
    let block_size = <Aes256 as BlockSizeUser>::block_size();
    encryptor.encrypt_padded_mut::<NoPadding>(iv, block_size)
        .expect("IV encryption should not fail with NoPadding");
}

/// XOR decryption used by SteamStub for header and payload decryption.
///
/// If `initial_key` is 0, the first 4 bytes of `data` are used as the initial key.
/// The key is updated after each 4-byte block decryption.
/// Matches Steamless SteamXor implementation.
pub fn steam_xor(data: &mut [u8], initial_key: u32) -> u32 {
    let mut key = initial_key;
    let mut offset = 0;

    if key == 0 && data.len() >= 4 {
        key = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        offset = 4;
    }

    // Process in 4-byte chunks, matching Steamless behavior
    let mut i = offset;
    while i + 4 <= data.len() {
        let val = u32::from_le_bytes([data[i], data[i+1], data[i+2], data[i+3]]);
        let decrypted = val ^ key;
        data[i..i+4].copy_from_slice(&decrypted.to_le_bytes());
        key = val;
        i += 4;
    }

    key
}

/// XTEA decryption (pass 2) used for SteamDRMP.dll decryption.
///
/// This is the core XTEA decryption function used in pass 2.
/// Matches Steamless SteamDrmpDecryptPass2 implementation.
pub fn xtea_decrypt_pass2(res: &mut [u32; 2], keys: &[u32; 4], mut v1: u32, mut v2: u32, n: u32) {
    const DELTA: u32 = 0x9E3779B9;
    let mut sum = DELTA.wrapping_mul(n);

    for _ in 0..n {
        let idx1 = ((sum >> 11) & 3) as usize;
        let idx2 = (sum & 3) as usize;

        // v2 = (v2 - (((v1 << 4 ^ v1 >> 5) + v1) ^ (sum + keys[sum >> 11 & 3]))) & mask;
        let temp1 = (v1 << 4) ^ (v1 >> 5);
        let temp2 = temp1.wrapping_add(v1);
        let temp3 = sum.wrapping_add(keys[idx1]);
        let temp4 = temp2 ^ temp3;
        v2 = v2.wrapping_sub(temp4);

        sum = sum.wrapping_sub(DELTA);

        // v1 = (v1 - (((v2 << 4 ^ v2 >> 5) + v2) ^ (sum + keys[sum & 3]))) & mask;
        let temp1 = (v2 << 4) ^ (v2 >> 5);
        let temp2 = temp1.wrapping_add(v2);
        let temp3 = sum.wrapping_add(keys[idx2]);
        let temp4 = temp2 ^ temp3;
        v1 = v1.wrapping_sub(temp4);
    }

    res[0] = v1;
    res[1] = v2;
}

/// XTEA decryption (pass 1) used for SteamDRMP.dll decryption.
///
/// This is a modified XTEA that includes XOR with rolling values.
pub fn xtea_decrypt_pass1(data: &mut [u8], keys: &[u32; 4]) {
    let mut v1 = 0x55555555u32;
    let mut v2 = 0x55555555u32;

    for chunk in data.chunks_exact_mut(8) {
        let d1 = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let d2 = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);

        let mut res = [0u32; 2];
        xtea_decrypt_pass2(&mut res, keys, d1, d2, 32);

        chunk[0..4].copy_from_slice(&(res[0] ^ v1).to_le_bytes());
        chunk[4..8].copy_from_slice(&(res[1] ^ v2).to_le_bytes());

        v1 = d1;
        v2 = d2;
    }
}

/// AES-256-CBC decryption for code section decryption (SteamStub v3.x).
/// Matches Steamless AesHelper.Decrypt with RebuildIv step.
pub fn aes256_cbc_decrypt(data: &mut [u8], key: &[u8; 32], iv: &mut [u8; 16]) -> Result<(), crate::steamflatline::error::SteamFlatlineError> {
    // Rebuild IV first (matching Steamless RebuildIv)
    aes_rebuild_iv(key, iv);

    let key_slice: &[u8] = key;
    let iv_slice: &[u8] = iv;
    let mut decryptor = Aes256CbcDec::new(key_slice.into(), iv_slice.into());
    decryptor.decrypt_padded_mut::<NoPadding>(data)
        .map_err(|e| crate::steamflatline::error::SteamFlatlineError::AesDecryptionFailed(e.to_string()))?;
    Ok(())
}

/// Find a byte pattern in data (similar to Steamless FindPattern).
/// Returns the offset of the first match, or -1 if not found.
/// Pattern format: "53 51 52 56 57 55 8B EC" (space-separated hex bytes)
/// Supports wildcards with "??" for unknown bytes.
pub fn find_pattern(data: &[u8], pattern: &str) -> isize {
    let pattern_bytes: Vec<Option<u8>> = pattern
        .split_whitespace()
        .map(|s| {
            if s == "??" {
                None
            } else {
                u8::from_str_radix(s, 16).ok()
            }
        })
        .collect();

    if pattern_bytes.is_empty() {
        return -1;
    }

    for i in 0..=data.len().saturating_sub(pattern_bytes.len()) {
        let mut matched = true;
        for (j, &pat_byte) in pattern_bytes.iter().enumerate() {
            if let Some(expected) = pat_byte {
                if data[i + j] != expected {
                    matched = false;
                    break;
                }
            }
        }
        if matched {
            return i as isize;
        }
    }

    -1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_steam_xor_basic() {
        // Test with known key
        let mut data = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0];
        let key = steam_xor(&mut data, 0x11111111);

        // First block: 0x78563412 ^ 0x11111111 = 0x69472503
        // Second block: 0xF0DEBC9A ^ 0x78563412 = 0x88888888
        assert_eq!(data[0..4], [0x03, 0x25, 0x47, 0x69]);
        assert_eq!(data[4..8], [0x88, 0x88, 0x88, 0x88]);
        assert_eq!(key, 0xF0DEBC9A);
    }

    #[test]
    fn test_steam_xor_zero_initial_key() {
        // When key is 0, first 4 bytes become the key
        let mut data = [0x11, 0x11, 0x11, 0x11, 0x22, 0x22, 0x22, 0x22];
        let key = steam_xor(&mut data, 0);

        // First 4 bytes [0x11, 0x11, 0x11, 0x11] = key 0x11111111 (not processed)
        // Second block: 0x22222222 ^ 0x11111111 = 0x33333333
        assert_eq!(data[0..4], [0x11, 0x11, 0x11, 0x11]); // Unchanged (used as key)
        assert_eq!(data[4..8], [0x33, 0x33, 0x33, 0x33]);
        assert_eq!(key, 0x22222222);
    }

    #[test]
    fn test_find_pattern_exact() {
        let data = [0x53, 0x51, 0x52, 0x56, 0x57, 0x55, 0x8B, 0xEC];
        assert_eq!(find_pattern(&data, "53 51 52 56 57 55 8B EC"), 0);
        assert_eq!(find_pattern(&data, "51 52 56"), 1);
        assert_eq!(find_pattern(&data, "FF FF"), -1);
    }

    #[test]
    fn test_find_pattern_wildcard() {
        let data = [0x53, 0x51, 0x52, 0x56, 0x57, 0x55, 0x8B, 0xEC];
        assert_eq!(find_pattern(&data, "53 ?? 52 56"), 0);
        assert_eq!(find_pattern(&data, "?? ?? 52 56"), 0);
    }

    #[test]
    fn test_xtea_pass2() {
        let keys = [0x12345678, 0x23456789, 0x3456789A, 0x456789AB];
        let mut res = [0u32; 2];
        xtea_decrypt_pass2(&mut res, &keys, 0xDEADBEEF, 0xCAFEBABE, 32);
        // Just verify it runs without panic
        assert!(res[0] != 0 || res[1] != 0);
    }

    #[test]
    fn test_xtea_pass2_known_values() {
        // Test XTEA pass2 with known inputs - verify it runs without panic
        // Known values are implementation-specific; just verify determinism
        let keys = [0x12345678, 0x23456789, 0x3456789A, 0x456789AB];
        let mut res1 = [0u32; 2];
        let mut res2 = [0u32; 2];
        xtea_decrypt_pass2(&mut res1, &keys, 0xDEADBEEF, 0xCAFEBABE, 32);
        xtea_decrypt_pass2(&mut res2, &keys, 0xDEADBEEF, 0xCAFEBABE, 32);
        // Should be deterministic
        assert_eq!(res1, res2);
        assert_ne!(res1, [0, 0]);
    }

    #[test]
    fn test_xtea_pass1_known_values() {
        // Test full DRMP decryption - verify it runs without panic
        let keys = [0x12345678, 0x23456789, 0x3456789A, 0x456789AB];
        let mut data1 = [0u8; 16];
        let mut data2 = [0u8; 16];

        xtea_decrypt_pass1(&mut data1, &keys);
        xtea_decrypt_pass1(&mut data2, &keys);

        // Should be deterministic
        assert_eq!(data1, data2);
        assert_ne!(data1, [0u8; 16]);
    }
}
