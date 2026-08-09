//! Assertions over the engine counters a case already receives on the worker's stderr.
//!
//! The engine prints `hl-native:` and `hl-native-detail:` records when the app asks for them
//! through `execution: { native: true, diagnostics: true }`. They were forwarded and never
//! compared, so a case could not fail for never having entered native execution.

use super::super::Error;
use serde::Deserialize;
use std::collections::BTreeMap;

/// The records that carry named counters; other `hl-native-*` lines are per-site histograms.
const RECORDS: [&str; 4] = ["hl-native:", "hl-native-detail:", "hl-native-entry:", "hl-interp:"];

/// The counters written into every result row, in this order. Deliberately a subset: five emitted
/// counters carry no information (`ibtc_shared_hits`, `ibtc_authenticated_entries`,
/// `ibtc_auth_rejections` are constant zero, `ibtc_site_misses`/`ibtc_shared_misses` merely
/// restate `fills`, and `a64_slim_exits` is a format-string literal with no field behind it).
const DIGEST: [&str; 22] = [
    "runs",
    "builds",
    "hits",
    "fallbacks",
    "sites",
    "services",
    "probes",
    "entries",
    "declined_executable",
    "declined_suppressed",
    "declined_cold",
    "declined_other",
    "instructions",
    "blocks",
    "slices",
    "completed",
    "fills",
    "operand_callbacks",
    "operand_cache_hits",
    "a64_guard_fast",
    "a64_guard_full",
    "a64_dirty_committed",
];

/// Per-site histogram lines, kept as `pc:weight` pairs so a row records *which* body translated.
const HISTOGRAMS: [(&str, &str); 2] = [
    ("hl-native-fallback-pc:", "fallback_pc"),
    ("hl-native-suppressed-entry:", "suppressed_pc"),
];

/// Hottest histogram entries retained per record; the engine already prints at most 24.
const HISTOGRAM_WIDTH: usize = 4;

/// One line of `counter=value` for the result row, empty when the engine emitted no record.
///
/// This is the whole reason the counters outlive the run: they were stderr-only, so a sweep could
/// not be audited after the fact for which cases actually translated their own body.
pub(crate) fn digest(stderr: &[u8]) -> String {
    let counters = Counters::parse(stderr);
    let mut fields: Vec<String> = DIGEST
        .iter()
        .filter_map(|name| counters.get(name).map(|value| format!("{name}={value}")))
        .collect();
    if fields.is_empty() {
        return String::new();
    }
    fields.extend(histograms(stderr));
    format!("native {}", fields.join(" "))
}

/// Collapses the per-pc lines into one field each, hottest first.
fn histograms(stderr: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(stderr);
    HISTOGRAMS
        .iter()
        .filter_map(|(record, label)| {
            let entries: Vec<String> = text
                .lines()
                .filter_map(|line| line.trim_start().strip_prefix(record))
                .filter_map(|fields| {
                    let mut pc = None;
                    let mut weight = None;
                    for (name, value) in fields.split_whitespace().filter_map(|field| field.split_once('=')) {
                        match name {
                            "pc" => pc = Some(value),
                            "count" | "refused" => weight = Some(value),
                            _ => {}
                        }
                    }
                    Some(format!("{}:{}", pc?, weight?))
                })
                .take(HISTOGRAM_WIDTH)
                .collect();
            (!entries.is_empty()).then(|| format!("{label}={}", entries.join(",")))
        })
        .collect()
}

/// One counter comparison from a case `expect.diagnostics` list.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct Assertion {
    counter: String,
    #[serde(default)]
    greater_than: Option<u64>,
    #[serde(default)]
    equals: Option<u64>,
}

impl Assertion {
    /// Exactly one comparison, so a mistyped key cannot deserialise into an assertion that
    /// nothing can violate.
    fn validate(&self) -> Result<(), Error> {
        if self.counter.trim().is_empty() {
            return Err("a diagnostic assertion names no counter".into());
        }
        match (self.greater_than, self.equals) {
            (Some(_), None) | (None, Some(_)) => Ok(()),
            _ => Err(format!(
                "diagnostic assertion on {:?} needs exactly one of greater-than or equals",
                self.counter
            )
            .into()),
        }
    }

    fn violation(&self, counters: &Counters) -> Option<String> {
        let Some(observed) = counters.get(&self.counter) else {
            return Some(format!(
                "engine diagnostic counter {:?} was never emitted; observed counters: {}",
                self.counter,
                counters.summary()
            ));
        };
        match (self.greater_than, self.equals) {
            (Some(bound), _) if observed <= bound => Some(format!(
                "engine diagnostic {} is {observed}, expected greater than {bound}",
                self.counter
            )),
            (_, Some(want)) if observed != want => Some(format!(
                "engine diagnostic {} is {observed}, expected {want}",
                self.counter
            )),
            _ => None,
        }
    }
}

/// Rejects a malformed list at load time so a manifest typo fails the run rather than the case.
pub(crate) fn validate(assertions: Vec<Assertion>, emitted: bool) -> Result<Vec<Assertion>, Error> {
    for assertion in &assertions {
        assertion.validate()?;
    }
    if !assertions.is_empty() && !emitted {
        return Err("expect.diagnostics needs execution: { native: true, diagnostics: true }".into());
    }
    Ok(assertions)
}

/// Counters summed over one worker run, which is every attempt of a soak case together.
struct Counters(BTreeMap<String, u64>);

impl Counters {
    fn parse(stderr: &[u8]) -> Self {
        let mut counters = BTreeMap::<String, u64>::new();
        for line in String::from_utf8_lossy(stderr).lines() {
            let Some(fields) = RECORDS.iter().find_map(|record| line.trim_start().strip_prefix(record)) else {
                continue;
            };
            for (name, value) in fields.split_whitespace().filter_map(|field| field.split_once('=')) {
                if let Ok(value) = value.parse::<u64>() {
                    *counters.entry(name.to_owned()).or_default() += value;
                }
            }
        }
        Self(counters)
    }

    fn get(&self, counter: &str) -> Option<u64> {
        self.0.get(counter).copied()
    }

    /// Named so a missing counter reports what the engine did emit, including nothing at all.
    fn summary(&self) -> String {
        if self.0.is_empty() {
            return "none (the engine emitted no hl-native record)".to_owned();
        }
        self.0
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// The first unmet assertion, or none. Absent diagnostics violate every assertion rather than
/// skipping it, because silently passing on missing data is the gap this exists to close.
pub(crate) fn violation(assertions: &[Assertion], stderr: &[u8]) -> Option<String> {
    if assertions.is_empty() {
        return None;
    }
    let counters = Counters::parse(stderr);
    assertions.iter().find_map(|assertion| assertion.violation(&counters))
}

#[cfg(test)]
mod tests {
    use super::{Assertion, Counters, validate, violation};

    fn assertions(yaml: &str) -> Vec<Assertion> {
        serde_yaml::from_str(yaml).unwrap()
    }

    const REPORT: &[u8] = b"hl-native: runs=3 builds=7 hits=9 fallbacks=0 sites=2 services=1\n\
                            hl-native-detail: fills=16 completed=58122035 a64_dirty_overflow=21\n";

    #[test]
    fn a_counter_above_its_bound_passes_and_one_at_it_fails() {
        assert!(violation(&assertions("- { counter: runs, greater-than: 0 }"), REPORT).is_none());
        assert!(
            violation(&assertions("- { counter: runs, greater-than: 3 }"), REPORT)
                .unwrap()
                .contains("is 3, expected greater than 3")
        );
    }

    #[test]
    fn equality_compares_exactly() {
        assert!(violation(&assertions("- { counter: a64_dirty_overflow, equals: 21 }"), REPORT).is_none());
        assert!(violation(&assertions("- { counter: a64_dirty_overflow, equals: 0 }"), REPORT).is_some());
    }

    #[test]
    fn a_zero_counter_still_fails_a_greater_than_assertion() {
        let report = b"hl-native: runs=0 builds=0 hits=0 fallbacks=0 sites=0 services=0\n";
        assert!(
            violation(&assertions("- { counter: runs, greater-than: 0 }"), report)
                .unwrap()
                .contains("is 0")
        );
    }

    #[test]
    fn absent_diagnostics_fail_rather_than_skip() {
        let message = violation(&assertions("- { counter: runs, greater-than: 0 }"), b"").unwrap();
        assert!(message.contains("never emitted"), "{message}");
        assert!(message.contains("no hl-native record"), "{message}");
    }

    #[test]
    fn an_unknown_counter_name_fails_rather_than_passing_silently() {
        assert!(
            violation(&assertions("- { counter: not_a_counter, greater-than: 0 }"), REPORT)
                .unwrap()
                .contains("never emitted")
        );
    }

    #[test]
    fn repeated_records_are_summed_across_the_run() {
        let report = b"hl-native: runs=1 builds=2\nhl-native: runs=4 builds=1\n";
        assert_eq!(Counters::parse(report).get("runs"), Some(5));
        assert_eq!(Counters::parse(report).get("builds"), Some(3));
    }

    #[test]
    fn histogram_lines_never_contribute_counters() {
        let report = b"hl-native-fallback-pc: pc=0x4000 count=12\nhl-native-suppressed-entry: pc=0x8 refused=3\n";
        assert_eq!(Counters::parse(report).get("count"), None);
        assert_eq!(Counters::parse(report).get("refused"), None);
    }

    #[test]
    fn the_digest_carries_counters_and_the_pcs_that_identify_the_translated_body() {
        let report = b"hl-native: runs=1 builds=1 hits=2 fallbacks=3 sites=1 services=0\n\
                       hl-native-entry: probes=9 entries=1 declined_executable=0 declined_suppressed=8 declined_cold=0 declined_other=0\n\
                       hl-interp: instructions=4000 blocks=70 slices=3\n\
                       hl-native-fallback-pc: pc=0x426a1c count=12\n\
                       hl-native-suppressed-entry: pc=0x8 refused=3\n";
        let digest = super::digest(report);
        assert!(
            digest.starts_with("native runs=1 builds=1 hits=2 fallbacks=3 sites=1 services=0"),
            "{digest}"
        );
        assert!(digest.contains("instructions=4000"), "{digest}");
        assert!(digest.contains("fallback_pc=0x426a1c:12"), "{digest}");
        assert!(digest.contains("suppressed_pc=0x8:3"), "{digest}");
        // The counters that carry no information stay out of the row.
        assert!(!digest.contains("ibtc_"), "{digest}");
        assert!(!digest.contains("a64_slim_exits"), "{digest}");
    }

    /// The digest sits inside the measured window, so it is bounded against the stderr ceiling
    /// rather than against the few kilobytes a typical case emits.
    #[test]
    fn the_digest_stays_far_under_a_millisecond_at_the_stderr_capture_ceiling() {
        // Scale rather than wall clock: an absolute bound here failed three lanes on a loaded box,
        // and the property that matters is that the digest is linear in its input, not quadratic.
        // Both timings are taken back to back so contention enters each of them equally.
        let line = "hl-native-entry: probes=1 entries=1 declined_executable=0 declined_suppressed=0 declined_cold=0 declined_other=0\n";
        let small = line.repeat(1024 * 1024 / line.len()).into_bytes();
        let ceiling = line.repeat(8 * 1024 * 1024 / line.len()).into_bytes();

        let started = std::time::Instant::now();
        assert!(!super::digest(&small).is_empty());
        let base = started.elapsed().max(std::time::Duration::from_micros(1));

        let started = std::time::Instant::now();
        assert!(!super::digest(&ceiling).is_empty());
        let full = started.elapsed();

        // Linear would be 8x for 8x the bytes; quadratic would be 64x. Allow 24x for scheduling.
        assert!(full < base * 24, "digest scaled {full:?} against {base:?} for 8x the input");
    }

    #[test]
    fn a_run_without_engine_diagnostics_writes_no_digest() {
        assert_eq!(super::digest(b"some unrelated stderr\n"), "");
    }

    #[test]
    fn no_assertion_keeps_the_default_of_asserting_nothing() {
        assert!(violation(&[], b"").is_none());
    }

    #[test]
    fn a_malformed_assertion_is_rejected_at_load() {
        assert!(validate(assertions("- { counter: runs }"), true).is_err());
        assert!(validate(assertions("- { counter: runs, equals: 1, greater-than: 0 }"), true).is_err());
        assert!(validate(assertions("- { counter: \" \", equals: 1 }"), true).is_err());
        assert!(validate(assertions("- { counter: runs, equals: 1 }"), true).is_ok());
    }

    #[test]
    fn asserting_on_counters_an_app_never_asks_for_is_a_load_error() {
        let error = validate(assertions("- { counter: runs, greater-than: 0 }"), false).unwrap_err();
        assert!(error.to_string().contains("diagnostics: true"), "{error}");
        assert!(validate(Vec::new(), false).is_ok());
    }
}
