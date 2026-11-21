//! Deterministic tape slugs (Issue #46, Story 4/5).
//!
//! A tape slug is the first 8 characters of `Crockford-Base32(SHA-256(natural
//! key))`. It is stable across runs and independent of the target database, so
//! a re-run produces the identical slug and the idempotent upsert matches the
//! same row. The slug is immutable once assigned (schema `tapes.slug UNIQUE`).

use sha2::{Digest, Sha256};

/// Crockford base32 alphabet (no `I`, `L`, `O`, `U`; uppercase).
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Length of a tape slug.
pub const SLUG_LEN: usize = 8;

/// Derives the stable 8-character slug for `natural_key`.
pub fn slug_from_key(natural_key: &str) -> String {
    let digest = Sha256::digest(natural_key.as_bytes());
    let mut out = String::with_capacity(SLUG_LEN);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for byte in digest.iter() {
        acc = (acc << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 0x1f) as usize] as char);
            if out.len() == SLUG_LEN {
                return out;
            }
        }
        acc &= (1u32 << bits) - 1;
    }
    // SHA-256 always yields >= 8 base32 characters; this is unreachable.
    out
}

/// True when `slug` is a valid 8-character Crockford base32 slug.
pub fn is_valid_slug(slug: &str) -> bool {
    slug.len() == SLUG_LEN
        && slug
            .bytes()
            .all(|b| ALPHABET.iter().any(|allowed| *allowed == b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn slug_is_eight_crockford_chars() {
        let slug = slug_from_key("rec-2024-01-01-ankd");
        assert_eq!(slug.len(), SLUG_LEN, "slug: {slug}");
        assert!(is_valid_slug(&slug), "slug: {slug}");
        assert!(!slug.chars().any(|c| "ILOU".contains(c)), "slug: {slug}");
    }

    #[test]
    fn slug_is_deterministic_and_conflict_free() {
        let a = slug_from_key("bc-2024-01-01-ankd");
        let b = slug_from_key("bc-2024-01-01-ankd");
        assert_eq!(a, b, "slug must be stable across runs");

        let keys = [
            "bc-2024-01-01-ankd",
            "rec-2024-01-02-duck",
            "rec-2024-01-03-momo",
            "rec-2024-01-04-x",
        ];
        let slugs: HashSet<String> = keys.iter().map(|k| slug_from_key(k)).collect();
        assert_eq!(slugs.len(), keys.len(), "fixture slugs must not collide");
    }
}
