//! Container-config request bits: bind-mount specs, stop-signal tokens, and query truthiness.
//! Self-contained: needs only the `libc` extern crate and `std`, so no `use super::*` glob.

/// Parse a `-v`/Binds spec `src:dst[:opts]` into `(host_source, container_dest, read_only)`. Docker
/// appends comma-separated options after the destination (e.g. `/h:/c:ro`, `vol:/c:rw,z`); `ro` marks
/// the mount read-only. Returns None for a malformed spec (no destination). Note: the prior code split
/// only on the FIRST colon, so `src:dst:ro` mounted at the literal path "dst:ro" — this fixes that and
/// surfaces the RW flag for inspect.
pub(crate) fn parse_bind(b: &str) -> Option<(&str, &str, bool)> {
    let mut it = b.splitn(3, ':');
    let src = it.next()?;
    let dst = it.next()?;
    let ro = it
        .next()
        .map(|o| o.split(',').any(|p| p == "ro"))
        .unwrap_or(false);
    if dst.is_empty() {
        return None;
    }
    Some((src, dst, ro))
}

/// Map a docker signal token ("SIGTERM"/"TERM"/"15"/"9"/"SIGKILL"/...) to its libc number.
/// Numeric tokens are taken verbatim; names are matched case-insensitively with or without the
/// "SIG" prefix. Anything unrecognised falls back to `default`.
pub(crate) fn parse_signal(s: &str, default: i32) -> i32 {
    let t = s.trim();
    if t.is_empty() {
        return default;
    }
    if let Ok(n) = t.parse::<i32>() {
        return n;
    }
    match t.to_ascii_uppercase().trim_start_matches("SIG") {
        "TERM" => libc::SIGTERM,
        "KILL" => libc::SIGKILL,
        "INT" => libc::SIGINT,
        "QUIT" => libc::SIGQUIT,
        "HUP" => libc::SIGHUP,
        "USR1" => libc::SIGUSR1,
        "USR2" => libc::SIGUSR2,
        "STOP" => libc::SIGSTOP,
        "CONT" => libc::SIGCONT,
        _ => default,
    }
}

pub(crate) fn q_truthy(s: &Option<String>) -> bool {
    matches!(s.as_deref(), Some("1") | Some("true") | Some("True"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_bind ---------------------------------------------------------
    #[test]
    fn bind_src_dst() {
        assert_eq!(parse_bind("/h:/c"), Some(("/h", "/c", false)));
    }
    #[test]
    fn bind_ro_flag() {
        assert_eq!(parse_bind("/h:/c:ro"), Some(("/h", "/c", true)));
        // `ro` may appear among a comma-list of options.
        assert_eq!(parse_bind("vol:/c:ro,z"), Some(("vol", "/c", true)));
    }
    #[test]
    fn bind_rw_flag() {
        assert_eq!(parse_bind("/h:/c:rw"), Some(("/h", "/c", false)));
        assert_eq!(parse_bind("/h:/c:rw,z"), Some(("/h", "/c", false)));
    }
    #[test]
    fn bind_empty_dst_is_none() {
        assert_eq!(parse_bind("/h:"), None);
    }
    #[test]
    fn bind_splitn3_keeps_extra_colons_in_opts() {
        // splitn(3, ':') means only the FIRST two colons split; the remainder is the opts field.
        // "a:b:c:d" -> src="a", dst="b", opts="c:d" (no "ro" -> false).
        assert_eq!(parse_bind("a:b:c:d"), Some(("a", "b", false)));
    }
    #[test]
    fn bind_no_colon_is_none() {
        assert_eq!(parse_bind("justsrc"), None);
    }

    // ---- parse_signal -------------------------------------------------------
    #[test]
    fn signal_named_with_prefix() {
        assert_eq!(parse_signal("SIGTERM", 0), libc::SIGTERM);
    }
    #[test]
    fn signal_named_without_prefix() {
        assert_eq!(parse_signal("TERM", 0), libc::SIGTERM);
        assert_eq!(parse_signal("kill", 0), libc::SIGKILL); // case-insensitive
    }
    #[test]
    fn signal_numeric_verbatim() {
        assert_eq!(parse_signal("15", 0), 15);
        assert_eq!(parse_signal("9", 0), 9);
    }
    #[test]
    fn signal_junk_falls_back_to_default() {
        assert_eq!(parse_signal("NOPE", 7), 7);
        assert_eq!(parse_signal("", 7), 7);
    }
}
