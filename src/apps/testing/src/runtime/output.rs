use super::{Error, diagnostic::Excerpt as _};
use std::io::Write;

pub(super) fn validate_profile(stderr: &str) -> Result<(), Error> {
    let mut crossings = None;
    let mut translations = None;
    for field in stderr
        .lines()
        .filter_map(|line| line.strip_prefix("[prof] "))
        .flat_map(str::split_whitespace)
    {
        let Some((name, value)) = field.split_once('=') else {
            continue;
        };
        let destination = match name {
            "crossings" => &mut crossings,
            "translations" => &mut translations,
            _ => continue,
        };
        *destination = Some(
            value
                .parse::<u64>()
                .map_err(|_| format!("retained C {name} is not an integer"))?,
        );
    }
    if crossings.is_none() || translations.is_none() {
        return Err("retained C profile omitted the crossings/translations summary".into());
    }
    Ok(())
}

pub(super) fn forward_profile(stderr: &str, mut output: impl Write) -> std::io::Result<()> {
    for line in stderr.lines().filter(|line| valid_profile_line(line)) {
        writeln!(output, "{line}")?;
    }
    Ok(())
}

pub(super) fn guest_stderr(stderr: &str) -> Vec<u8> {
    stderr
        .lines()
        .filter(|line| !line.starts_with("[prof] ") && !line.starts_with("[diag] "))
        .flat_map(|line| [line.as_bytes(), b"\n"].concat())
        .collect()
}

fn valid_profile_line(line: &str) -> bool {
    let Some(fields) = line.strip_prefix("[prof] ") else {
        return false;
    };
    fields.split_whitespace().any(|field| {
        field
            .strip_prefix("crossings=")
            .is_some_and(|value| value.parse::<u64>().is_ok())
    }) && fields.split_whitespace().any(|field| {
        field
            .strip_prefix("translations=")
            .is_some_and(|value| value.parse::<u64>().is_ok())
    })
}

/// Declared stderr patterns are an assertion, not an allowance: every emitted line must match a
/// declared pattern, and every declared pattern must match a line.
pub(super) fn stderr_violation(patterns: &[String], stderr: &[u8]) -> Option<String> {
    if patterns.is_empty() {
        return (!stderr.is_empty()).then(|| format!("unexpected stderr: {}", stderr.preview()));
    }
    let Ok(text) = std::str::from_utf8(stderr) else {
        return Some(format!("stderr is not UTF-8: {}", stderr.preview()));
    };
    let lines = text.lines().collect::<Vec<_>>();
    if let Some(line) = lines
        .iter()
        .find(|line| !patterns.iter().any(|pattern| glob(pattern, line)))
    {
        return Some(format!("undeclared stderr line: {line:?}"));
    }
    patterns
        .iter()
        .find(|pattern| !lines.iter().any(|line| glob(pattern, line)))
        .map(|pattern| format!("expected stderr pattern never appeared: {pattern:?}"))
}

/// `*` matches any run of characters; every other character is literal and the match is anchored.
fn glob(pattern: &str, text: &str) -> bool {
    let Some((head, rest)) = pattern.split_once('*') else {
        return pattern == text;
    };
    let Some(mut tail) = text.strip_prefix(head) else {
        return false;
    };
    loop {
        if glob(rest, tail) {
            return true;
        }
        if tail.is_empty() {
            return false;
        }
        let mut rest_of_tail = tail.chars();
        rest_of_tail.next();
        tail = rest_of_tail.as_str();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatcher_summary_is_a_complete_diagnostic_record() {
        validate_profile("[prof] dispatcher crossings=41 translations=7\n").unwrap();
    }

    #[test]
    fn process_exit_details_are_optional_but_the_summary_is_not() {
        validate_profile("[prof] crossings=41 syscalls=9 ibtc_miss=2 translations=7\n").unwrap();
        let error = validate_profile("[prof] shadow_push=3 shret_hit=2\n").unwrap_err();
        assert!(error.to_string().contains("crossings/translations"), "{error}");
    }

    #[test]
    fn profile_records_cross_the_worker_boundary_without_guest_stderr() {
        let mut forwarded = Vec::new();
        forward_profile(
            "guest warning\n[prof] crossings=41 translations=7\n[prof] dispatcher crossings=42 translations=8\n",
            &mut forwarded,
        )
        .unwrap();
        assert_eq!(
            forwarded,
            b"[prof] crossings=41 translations=7\n[prof] dispatcher crossings=42 translations=8\n"
        );
        assert!(valid_profile_line("[prof] crossings=41 translations=7"));
        assert!(!valid_profile_line("[prof] forged guest text"));
        assert_eq!(
            guest_stderr("guest warning\n[diag] boundary samples=7\n[prof] crossings=41 translations=7\n"),
            b"guest warning\n"
        );
    }

    #[test]
    fn an_undeclared_stderr_line_still_fails_the_case() {
        let patterns = vec!["fdrss base=*KB fin=*KB grew=*KB thresh=122880KB".to_owned()];
        assert!(stderr_violation(&patterns, b"fdrss base=1KB fin=1KB grew=0KB thresh=122880KB\n").is_none());
        let noisy = b"fdrss base=1KB fin=1KB grew=0KB thresh=122880KB\nhl: internal fault\n";
        assert!(
            stderr_violation(&patterns, noisy)
                .unwrap()
                .contains("undeclared stderr line")
        );
    }

    #[test]
    fn a_pattern_that_never_appears_fails_rather_than_passing_silently() {
        let patterns = vec!["A both".to_owned(), "Z done".to_owned()];
        assert!(
            stderr_violation(&patterns, b"A both\n")
                .unwrap()
                .contains("never appeared")
        );
    }

    #[test]
    fn no_declared_pattern_keeps_the_empty_stderr_default() {
        assert!(stderr_violation(&[], b"").is_none());
        assert!(stderr_violation(&[], b"anything").is_some());
    }

    #[test]
    fn wildcards_are_anchored_and_literal_elsewhere() {
        assert!(glob("a*c", "abbbc"));
        assert!(glob("a*c", "ac"));
        assert!(!glob("a*c", "abbbcd"));
        assert!(!glob("abc", "abcd"));
        assert!(glob("[cache-reuse] kind=*", "[cache-reuse] kind=fork"));
    }
}
