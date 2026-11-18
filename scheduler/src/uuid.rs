//! UUIDv7 generation for local IDs.
//!
//! Local primary keys are application-generated canonical UUIDv7 strings
//! (36 characters, lowercase, hyphenated). Values are time-sortable and
//! strictly increasing across calls within the process, so ordering by ID
//! matches insertion order.

use uuid::Uuid;

/// Generates a canonical UUIDv7 string: 36 characters, lowercase, hyphenated.
///
/// Values are strictly increasing across all calls in this process, including
/// calls that happen within the same millisecond.
pub fn new_id() -> String {
    Uuid::now_v7().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Version;

    #[test]
    fn is_canonical_36_chars_lowercase() {
        let id = new_id();
        assert_eq!(id.len(), 36, "unexpected length: {id}");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "non-canonical characters: {id}"
        );
        let parts: Vec<&str> = id.split('-').collect();
        let lens: Vec<usize> = parts.iter().map(|p| p.len()).collect();
        assert_eq!(lens, vec![8, 4, 4, 4, 12], "unexpected grouping: {id}");
    }

    #[test]
    fn is_parseable_v7() {
        let id = new_id();
        let parsed = Uuid::parse_str(&id).expect("generated id must parse");
        assert_eq!(parsed.get_version(), Some(Version::SortRand));
        assert_eq!(parsed.get_version_num(), 7);
    }

    #[test]
    fn variant_is_rfc4122() {
        let id = new_id();
        let parsed = Uuid::parse_str(&id).unwrap();
        assert_eq!(parsed.get_variant(), uuid::Variant::RFC4122);
    }

    #[test]
    fn strictly_increasing_over_10k_calls() {
        let mut prev = new_id();
        for i in 0..10_000 {
            let cur = new_id();
            assert!(
                prev < cur,
                "not strictly increasing at index {i}: {prev} !< {cur}"
            );
            prev = cur;
        }
    }
}
