//! Pure HTTP `Range` / `If-Range` parsing for the audio proxy (`docs/tapes.md` §3.7).
//!
//! None of these functions perform I/O: the handler decides `200`/`206`/`416`
//! solely from the returned [`ByteRange`], which keeps the whole matrix
//! unit-testable without a network.

/// The outcome of parsing a client `Range` header against a known size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// Serve the full representation (`200`, or a full body regardless of the
    /// header: missing header, unknown unit, multi-range, unparsable).
    Full,
    /// Serve the inclusive `[start, end]` slice (`206`).
    Satisfiable { start: u64, end: u64 },
    /// The requested range lies outside the file (`416`).
    Unsatisfiable,
}

/// Parses a single-range `Range` header (§3.7 matrix).
///
/// - missing / not `bytes=` / multi-range (`,`)/ unparsable → [`ByteRange::Full`]
/// - `bytes=N-` (N ≥ size) → [`ByteRange::Unsatisfiable`], else `end = size-1`
/// - `bytes=-N`: N = 0 → [`ByteRange::Unsatisfiable`], N ≥ size → whole file
/// - `bytes=a-b`: a > b or a ≥ size → [`ByteRange::Unsatisfiable`], b clamped
pub fn parse_range(raw: Option<&str>, size: u64) -> ByteRange {
    let Some(raw) = raw else {
        return ByteRange::Full;
    };
    let Some(spec) = raw.trim().strip_prefix("bytes=") else {
        return ByteRange::Full;
    };
    let spec = spec.trim();
    // Multi-range and anything without exactly one `-` is served in full (D10).
    if spec.contains(',') {
        return ByteRange::Full;
    }
    let Some((first, last)) = spec.split_once('-') else {
        return ByteRange::Full;
    };
    let first = first.trim();
    let last = last.trim();

    if first.is_empty() {
        // Suffix range: `bytes=-N` → the last N bytes.
        let Ok(n) = last.parse::<u64>() else {
            return ByteRange::Full;
        };
        if n == 0 || size == 0 {
            return ByteRange::Unsatisfiable;
        }
        return ByteRange::Satisfiable {
            start: size.saturating_sub(n),
            end: size - 1,
        };
    }

    let Ok(start) = first.parse::<u64>() else {
        return ByteRange::Full;
    };
    if start >= size {
        return ByteRange::Unsatisfiable;
    }

    if last.is_empty() {
        // Open-ended: `bytes=N-` → from N to the end.
        return ByteRange::Satisfiable {
            start,
            end: size - 1,
        };
    }

    let Ok(end) = last.parse::<u64>() else {
        return ByteRange::Full;
    };
    if start > end {
        return ByteRange::Unsatisfiable;
    }
    ByteRange::Satisfiable {
        start,
        end: end.min(size - 1),
    }
}

/// Strong `If-Range` comparison: a missing header always allows the range,
/// otherwise the value must equal the current ETag exactly (weak validators
/// never match).
pub fn if_range_allows(if_range: Option<&str>, etag: &str) -> bool {
    match if_range {
        None => true,
        Some(raw) => raw.trim() == etag,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: u64 = 8192;

    #[test]
    fn missing_or_non_bytes_unit_serves_full() {
        assert_eq!(parse_range(None, SIZE), ByteRange::Full);
        assert_eq!(parse_range(Some("items=0-1"), SIZE), ByteRange::Full);
        assert_eq!(parse_range(Some("bytes=abc"), SIZE), ByteRange::Full);
        assert_eq!(parse_range(Some("bytes="), SIZE), ByteRange::Full);
    }

    #[test]
    fn multi_range_serves_full() {
        assert_eq!(
            parse_range(Some("bytes=0-1023,2000-3023"), SIZE),
            ByteRange::Full
        );
    }

    #[test]
    fn closed_range_is_satisfiable() {
        assert_eq!(
            parse_range(Some("bytes=0-1023"), SIZE),
            ByteRange::Satisfiable {
                start: 0,
                end: 1023
            }
        );
    }

    #[test]
    fn open_ended_range_runs_to_eof() {
        assert_eq!(
            parse_range(Some("bytes=1000-"), SIZE),
            ByteRange::Satisfiable {
                start: 1000,
                end: SIZE - 1
            }
        );
    }

    #[test]
    fn suffix_range_takes_last_bytes() {
        assert_eq!(
            parse_range(Some("bytes=-4096"), SIZE),
            ByteRange::Satisfiable {
                start: SIZE - 4096,
                end: SIZE - 1
            }
        );
        // N ≥ size → the whole file.
        assert_eq!(
            parse_range(Some("bytes=-999999"), SIZE),
            ByteRange::Satisfiable {
                start: 0,
                end: SIZE - 1
            }
        );
        // N = 0 is unsatisfiable.
        assert_eq!(
            parse_range(Some("bytes=-0"), SIZE),
            ByteRange::Unsatisfiable
        );
    }

    #[test]
    fn start_at_or_past_size_is_unsatisfiable() {
        assert_eq!(
            parse_range(Some("bytes=8192-"), SIZE),
            ByteRange::Unsatisfiable
        );
        assert_eq!(
            parse_range(Some("bytes=9000-9100"), SIZE),
            ByteRange::Unsatisfiable
        );
    }

    #[test]
    fn inverted_range_is_unsatisfiable() {
        assert_eq!(
            parse_range(Some("bytes=200-100"), SIZE),
            ByteRange::Unsatisfiable
        );
    }

    #[test]
    fn end_is_clamped_to_last_byte() {
        assert_eq!(
            parse_range(Some("bytes=0-99999"), SIZE),
            ByteRange::Satisfiable {
                start: 0,
                end: SIZE - 1
            }
        );
    }

    #[test]
    fn zero_size_file_has_no_satisfiable_range() {
        assert_eq!(parse_range(Some("bytes=0-"), 0), ByteRange::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=-1"), 0), ByteRange::Unsatisfiable);
    }

    #[test]
    fn if_range_strong_comparison() {
        let etag = "\"abc\"";
        assert!(if_range_allows(None, etag));
        assert!(if_range_allows(Some("\"abc\""), etag));
        assert!(!if_range_allows(Some("\"def\""), etag));
        // Weak validators never match a strong If-Range.
        assert!(!if_range_allows(Some("W/\"abc\""), etag));
    }
}
