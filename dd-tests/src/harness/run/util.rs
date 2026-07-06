/// Single-quote a string for safe inclusion in the mac-side `bash -lc` launch script (used to append
/// the stdout-drain redirect target). Mirrors `SpawnConfig::shq`.
pub(super) fn shq(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('\'');
    for c in s.chars() {
        if c == '\'' {
            o.push_str("'\\''");
        } else {
            o.push(c);
        }
    }
    o.push('\'');
    o
}

/// Drop the JIT's diagnostic "unhandled syscall ..." lines so they don't pollute stdout checks.
pub(super) fn strip_noise(b: &[u8]) -> String {
    String::from_utf8_lossy(b)
        .lines()
        .filter(|l| !l.contains("unhandled syscall"))
        .collect::<Vec<_>>()
        .join("\n")
        + if b.ends_with(b"\n") && !b.is_empty() {
            "\n"
        } else {
            ""
        }
}

#[cfg(test)]
mod tests {
    use super::{shq, strip_noise};

    // ── shq(): single-quote shell escaping ───────────────────────────────────
    #[test]
    fn shq_wraps_plain_string_in_single_quotes() {
        assert_eq!(shq("plain"), "'plain'");
    }

    #[test]
    fn shq_escapes_embedded_single_quote() {
        // each ' becomes the close-quote / escaped-quote / reopen-quote form: '\''
        assert_eq!(shq("a'b"), "'a'\\''b'");
    }

    #[test]
    fn shq_empty_string_is_empty_quotes() {
        assert_eq!(shq(""), "''");
    }

    #[test]
    fn shq_preserves_path_like_content() {
        assert_eq!(shq("/tmp/x y.out"), "'/tmp/x y.out'");
    }

    // ── strip_noise(): drop "unhandled syscall" lines, keep trailing newline ──
    #[test]
    fn strip_noise_removes_unhandled_syscall_lines() {
        let out = strip_noise(b"hello\nunhandled syscall 42\nworld\n");
        assert_eq!(out, "hello\nworld\n");
    }

    #[test]
    fn strip_noise_preserves_trailing_newline_on_nonempty() {
        assert_eq!(strip_noise(b"a\n"), "a\n");
    }

    #[test]
    fn strip_noise_keeps_no_trailing_newline_when_absent() {
        assert_eq!(strip_noise(b"a"), "a");
    }

    #[test]
    fn strip_noise_empty_input_stays_empty() {
        // empty input: no lines, and the `!b.is_empty()` guard blocks a spurious "\n".
        assert_eq!(strip_noise(b""), "");
    }

    #[test]
    fn strip_noise_multiline_no_trailing_newline() {
        // no trailing newline in → none out, even across multiple lines.
        assert_eq!(strip_noise(b"one\ntwo"), "one\ntwo");
    }
}
