//! Players' download codes. Only their hashes are stored.

use rand::Rng;
use rand::distr::Alphanumeric;
use sha2::{Digest, Sha256};

const CODE_LENGTH: usize = 32;

/// Codes are long and random, so a plain hash keeps a leaked store useless. Surrounding spaces
/// are ignored: a code pasted with a newline still works.
pub fn hash_code(code: &str) -> String {
    Sha256::digest(code.trim().as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A fresh code: 32 letters and digits, about 190 bits.
pub fn new_code() -> String {
    rand::rng()
        .sample_iter(Alphanumeric)
        .take(CODE_LENGTH)
        .map(char::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surrounding_spaces_do_not_change_the_hash() {
        assert_eq!(hash_code(" abc\n"), hash_code("abc"));
    }

    #[test]
    fn the_hash_is_plain_sha256_hex() {
        assert_eq!(
            hash_code("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn codes_are_long_and_differ() {
        let (first, second) = (new_code(), new_code());
        assert_eq!(first.len(), CODE_LENGTH);
        assert_ne!(first, second);
    }
}
