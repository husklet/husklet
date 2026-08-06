//! Bounded, human-readable renderings of guest output for failure diagnostics.

/// Bytes of context shown on each side of the first difference.
const CONTEXT: usize = 48;
/// Longest escaped excerpt emitted for one byte stream.
const EXCERPT: usize = 320;
/// Longest diagnostic any single case may contribute to a result row.
pub(super) const DIAGNOSTIC_LIMIT: usize = 4 * 1024;

/// Describes where two byte streams first diverge, with escaped context from both sides.
pub(super) fn compare(label: &str, got: &[u8], expected: &[u8]) -> String {
    let offset = got
        .iter()
        .zip(expected)
        .position(|(left, right)| left != right)
        .unwrap_or_else(|| got.len().min(expected.len()));
    let start = offset.saturating_sub(CONTEXT);
    format!(
        "{label} differs: got {} bytes, expected {}; first difference at offset {offset} ({}); got[{start}..]={}; expected[{start}..]={}",
        got.len(),
        expected.len(),
        at(got, expected, offset),
        window(got, start),
        window(expected, start)
    )
}

/// Escapes and bounds an arbitrary byte stream for inclusion in a one-line diagnostic.
pub(super) fn preview(bytes: &[u8]) -> String {
    window(bytes, 0)
}

/// Bounds an assembled diagnostic, marking any bytes it dropped.
pub(super) fn bound(text: String, limit: usize) -> String {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit.saturating_sub(32);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let dropped = text.len() - end;
    format!("{}[+{dropped} bytes truncated]", &text[..end])
}

fn at(got: &[u8], expected: &[u8], offset: usize) -> String {
    match (got.get(offset), expected.get(offset)) {
        (Some(left), Some(right)) => format!("got {left:#04x} {:?}, expected {right:#04x} {:?}", *left as char, *right as char),
        (Some(left), None) => format!("got {left:#04x} {:?}, expected end of output", *left as char),
        (None, Some(right)) => format!("output ended, expected {right:#04x} {:?}", *right as char),
        (None, None) => "streams are identical".to_owned(),
    }
}

fn window(bytes: &[u8], start: usize) -> String {
    let tail = bytes.get(start..).unwrap_or_default();
    let mut text = String::with_capacity(EXCERPT);
    let mut consumed = 0;
    for byte in tail {
        let escaped = byte.escape_ascii().to_string();
        if text.len() + escaped.len() > EXCERPT {
            break;
        }
        text.push_str(&escaped);
        consumed += 1;
    }
    let remaining = tail.len() - consumed;
    if remaining == 0 {
        format!("\"{text}\"")
    } else {
        format!("\"{text}\"[+{remaining} bytes]")
    }
}

#[cfg(test)]
mod tests {
    use super::{DIAGNOSTIC_LIMIT, bound, compare, preview};

    #[test]
    fn first_difference_is_located_and_both_sides_are_shown() {
        let text = compare("stdout", b"acc=6337.584", b"acc=6337.834");
        assert!(text.contains("first difference at offset 9"), "{text}");
        assert!(text.contains("acc=6337.584"), "{text}");
        assert!(text.contains("acc=6337.834"), "{text}");
    }

    #[test]
    fn truncation_is_bounded_marked_and_delimiter_free() {
        let got = vec![b'\n'; 1 << 16];
        let text = compare("stdout", &got, b"");
        assert!(text.len() < DIAGNOSTIC_LIMIT, "{}", text.len());
        assert!(!text.contains(['\t', '\n']));
        assert!(text.contains("bytes]"), "{text}");
        assert!(preview(&got).contains("\\n"));
    }

    #[test]
    fn a_short_diagnostic_survives_bounding_unchanged() {
        assert_eq!(bound("short".to_owned(), 64), "short");
        let bounded = bound("x".repeat(200), 100);
        assert!(bounded.len() <= 100 && bounded.contains("truncated"), "{bounded}");
    }

    #[test]
    fn a_truncated_stream_reports_the_missing_tail() {
        let text = compare("stdout", b"abc", b"abcdef");
        assert!(text.contains("output ended"), "{text}");
    }
}
