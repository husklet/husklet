use super::{Error, diagnostic::Excerpt as _};
use std::collections::BTreeMap;
use std::io::Write;

const BACKEND_TREE_PREFIX: &str = "[diag] backend-tree ";
const BACKEND_TREE_FIELDS: [&str; 33] = [
    "version",
    "root_pid",
    "claimed",
    "completed",
    "abnormal",
    "missing",
    "duplicate_finalize",
    "crossings",
    "translated_entries",
    "interpreted_entries",
    "translated_steps",
    "interpreted_steps",
    "translations",
    "map_hits",
    "stw_retries",
    "irq_pending",
    "reason0",
    "reason1",
    "reason2",
    "reason3",
    "reason4",
    "reason5",
    "reason6",
    "reason7",
    "reason8",
    "reason9",
    "reason10",
    "reason11",
    "reason12",
    "reason13",
    "reason14",
    "reason15",
    "reason_other",
];

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

pub(super) fn validate_backend_tree(stderr: &[u8], enabled: bool) -> Result<(), Error> {
    let records = stderr
        .split(|byte| *byte == b'\n')
        .filter(|line| line.starts_with(BACKEND_TREE_PREFIX.as_bytes()))
        .count();
    let expected = usize::from(enabled);
    if records != expected {
        return Err(format!("backend-tree diagnostic appeared {records} times, expected {expected}").into());
    }
    if !enabled {
        return Ok(());
    }
    let text = std::str::from_utf8(stderr).map_err(|_| "backend-tree diagnostic stderr is not UTF-8")?;
    let _ = backend_tree(text)?;
    Ok(())
}

fn backend_tree(stderr: &str) -> Result<Option<BTreeMap<&str, u64>>, Error> {
    let records = stderr
        .lines()
        .filter_map(|line| line.strip_prefix(BACKEND_TREE_PREFIX))
        .collect::<Vec<_>>();
    if records.is_empty() {
        return Ok(None);
    }
    if records.len() != 1 {
        return Err(format!(
            "backend-tree diagnostic appeared {} times, expected once",
            records.len()
        )
        .into());
    }
    let mut fields = BTreeMap::new();
    for field in records[0].split_whitespace() {
        let Some((name, value)) = field.split_once('=') else {
            return Err(format!("backend-tree diagnostic has malformed field {field:?}").into());
        };
        if !BACKEND_TREE_FIELDS.contains(&name) {
            return Err(format!("backend-tree diagnostic has unknown field {name:?}").into());
        }
        let value = value
            .parse::<u64>()
            .map_err(|_| format!("backend-tree field {name:?} is not an integer"))?;
        if fields.insert(name, value).is_some() {
            return Err(format!("backend-tree diagnostic duplicates field {name:?}").into());
        }
    }
    for name in BACKEND_TREE_FIELDS {
        if !fields.contains_key(name) {
            return Err(format!("backend-tree diagnostic omitted field {name:?}").into());
        }
    }
    if fields["version"] != 1 || fields["root_pid"] == 0 {
        return Err("backend-tree diagnostic has invalid version or root pid".into());
    }
    let lifecycle = fields["completed"]
        .checked_add(fields["abnormal"])
        .and_then(|value| value.checked_add(fields["missing"]));
    if lifecycle != Some(fields["claimed"]) {
        return Err("backend-tree lifecycle totals do not reconcile".into());
    }
    if fields["translated_entries"].checked_add(fields["interpreted_entries"]) != Some(fields["crossings"]) {
        return Err("backend-tree entry totals do not reconcile with crossings".into());
    }
    let reasons = (0..16).try_fold(0_u64, |total, reason| {
        total.checked_add(fields[format!("reason{reason}").as_str()])
    });
    let reasons = reasons.and_then(|total| total.checked_add(fields["reason_other"]));
    if reasons != Some(fields["crossings"]) {
        return Err("backend-tree reason totals do not reconcile with crossings".into());
    }
    Ok(Some(fields))
}

pub(crate) fn backend_tree_digest(stderr: &[u8]) -> String {
    let Ok(text) = std::str::from_utf8(stderr) else {
        return String::new();
    };
    let Ok(Some(fields)) = backend_tree(text) else {
        return String::new();
    };
    format!(
        "backend-tree claimed={} completed={} abnormal={} missing={} duplicate_finalize={} crossings={} translated_entries={} interpreted_entries={} translated_steps={} interpreted_steps={}",
        fields["claimed"],
        fields["completed"],
        fields["abnormal"],
        fields["missing"],
        fields["duplicate_finalize"],
        fields["crossings"],
        fields["translated_entries"],
        fields["interpreted_entries"],
        fields["translated_steps"],
        fields["interpreted_steps"]
    )
}

pub(super) fn forward_profile(stderr: &str, mut output: impl Write) -> std::io::Result<()> {
    for line in stderr
        .lines()
        .filter(|line| valid_profile_line(line) || line.starts_with(BACKEND_TREE_PREFIX))
    {
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
        validate_backend_tree(b"ordinary guest stderr\n", false).unwrap();
    }

    const TREE: &str = "[diag] backend-tree version=1 root_pid=42 claimed=3 completed=1 abnormal=1 missing=1 duplicate_finalize=0 crossings=5 translated_entries=2 interpreted_entries=3 translated_steps=8 interpreted_steps=13 translations=2 map_hits=3 stw_retries=0 irq_pending=1 reason0=2 reason1=1 reason2=0 reason3=0 reason4=0 reason5=1 reason6=0 reason7=0 reason8=0 reason9=0 reason10=0 reason11=0 reason12=0 reason13=0 reason14=0 reason15=0 reason_other=1\n";

    #[test]
    fn backend_tree_record_is_exact_and_reconciled() {
        validate_backend_tree(TREE.as_bytes(), true).unwrap();
        let digest = backend_tree_digest(TREE.as_bytes());
        assert!(digest.contains("claimed=3 completed=1"), "{digest}");
        assert!(
            digest.contains("crossings=5 translated_entries=2 interpreted_entries=3"),
            "{digest}"
        );
    }

    #[test]
    fn backend_tree_rejects_missing_duplicate_unknown_and_unreconciled_fields() {
        let profile = |tree: &str| format!("[prof] crossings=1 translations=1\n{tree}");
        assert!(
            validate_backend_tree(b"[prof] crossings=1 translations=1\n", true)
                .unwrap_err()
                .to_string()
                .contains("appeared 0 times")
        );
        assert!(
            validate_backend_tree(TREE.as_bytes(), false)
                .unwrap_err()
                .to_string()
                .contains("expected 0")
        );
        let missing = TREE.replace(" map_hits=3", "");
        assert!(
            validate_backend_tree(profile(&missing).as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("omitted field")
        );
        let duplicate = TREE.replace(" map_hits=3", " map_hits=3 map_hits=3");
        assert!(
            validate_backend_tree(profile(&duplicate).as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("duplicates field")
        );
        assert!(
            validate_backend_tree(
                format!("[prof] crossings=1 translations=1\n{TREE}{TREE}").as_bytes(),
                true
            )
            .unwrap_err()
            .to_string()
            .contains("appeared 2 times")
        );
        let unknown = TREE.replace(" map_hits=3", " map_hits=3 mystery=9");
        assert!(
            validate_backend_tree(profile(&unknown).as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("unknown field")
        );
        let entries = TREE.replace(" translated_entries=2", " translated_entries=1");
        assert!(
            validate_backend_tree(profile(&entries).as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("entry totals")
        );
        let reasons = TREE.replace(" reason_other=1", " reason_other=0");
        assert!(
            validate_backend_tree(profile(&reasons).as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("reason totals")
        );
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
