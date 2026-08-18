//! Assertions over the engine counters a case already receives on the worker's stderr.
//!
//! The engine prints backend-owned records when the app asks for them through
//! `execution: { native: true, diagnostics: true }`. They were forwarded and never
//! compared, so a case could not fail for never having entered native execution.

use super::super::Error;
use serde::Deserialize;
use std::collections::BTreeMap;

/// The records that carry named counters; other `hl-native-*` lines are per-site histograms.
const RECORDS: [&str; 6] = [
    "[prof]",
    "hl-c:",
    "hl-native:",
    "hl-native-detail:",
    "hl-native-entry:",
    "hl-interp:",
];

/// The counters written into every result row, in this order. Deliberately a subset: five emitted
/// counters carry no information (`ibtc_shared_hits`, `ibtc_authenticated_entries`,
/// `ibtc_auth_rejections` are constant zero, `ibtc_site_misses`/`ibtc_shared_misses` merely
/// restate `fills`, and `a64_slim_exits` is a format-string literal with no field behind it).
const DIGEST: [&str; 32] = [
    "crossings",
    "translations",
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
    "x86_guard_fast",
    "x86_guard_full",
    "x86_dirty_committed",
    "x86_dirty_merged",
    "x86_write_cache_hit",
    "x86_write_cache_miss",
    "x86_dirty_overflow",
    "x86_guard_fallback",
    "a64_dirty_overflow",
    "a64_guard_fallback",
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
    let backend = if String::from_utf8_lossy(stderr).lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("[prof]") || line.starts_with("hl-c:")
    }) {
        "c"
    } else {
        "native"
    };
    format!("{backend} {}", fields.join(" "))
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
                .filter_map(histogram_entry)
                .take(HISTOGRAM_WIDTH)
                .collect();
            (!entries.is_empty()).then(|| format!("{label}={}", entries.join(",")))
        })
        .collect()
}

/// The `pc:weight` pair a histogram record carries, or nothing when the record names neither.
///
/// `count` and `refused` are the two spellings the engine uses for the same weight.
fn histogram_entry(fields: &str) -> Option<String> {
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
}

/// One engine diagnostic assertion from a case `expect.diagnostics` list.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Assertion {
    Counter(CounterAssertion),
    Record(RecordAssertion),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct CounterAssertion {
    counter: String,
    #[serde(default)]
    greater_than: Option<u64>,
    #[serde(default)]
    equals: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct RecordAssertion {
    record: String,
    count: usize,
    #[serde(default)]
    fields: BTreeMap<String, String>,
    #[serde(default)]
    nonnegative: Vec<String>,
}

impl CounterAssertion {
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

impl RecordAssertion {
    fn validate(&self) -> Result<(), Error> {
        if self.record.trim().is_empty() || self.count == 0 {
            return Err("a diagnostic record assertion needs a name and nonzero count".into());
        }
        if self
            .fields
            .keys()
            .chain(&self.nonnegative)
            .any(|field| field.trim().is_empty() || field.contains('='))
        {
            return Err(format!("diagnostic record {:?} has an invalid field name", self.record).into());
        }
        Ok(())
    }

    fn violation(&self, stderr: &[u8]) -> Option<String> {
        let records = String::from_utf8_lossy(stderr)
            .lines()
            .filter_map(|line| {
                let fields = line.trim_start().strip_prefix(&self.record)?;
                fields
                    .chars()
                    .next()
                    .is_some_and(char::is_whitespace)
                    .then_some(fields)
            })
            .collect::<Vec<_>>();
        if records.len() != self.count {
            return Some(format!(
                "engine diagnostic record {:?} appeared {} times, expected {}",
                self.record,
                records.len(),
                self.count
            ));
        }
        for record in records {
            let Some(fields) = record_fields(record) else {
                return Some(format!(
                    "engine diagnostic record {:?} contains a malformed or duplicate field",
                    self.record
                ));
            };
            for (name, expected) in &self.fields {
                if fields.get(name.as_str()).copied() != Some(expected.as_str()) {
                    return Some(format!(
                        "engine diagnostic record {:?} field {name:?} is {:?}, expected {expected:?}",
                        self.record,
                        fields.get(name.as_str())
                    ));
                }
            }
            for name in &self.nonnegative {
                let Some(value) = fields.get(name.as_str()).and_then(|value| value.parse::<i128>().ok()) else {
                    return Some(format!(
                        "engine diagnostic record {:?} field {name:?} is absent or not an integer",
                        self.record
                    ));
                };
                if value < 0 {
                    return Some(format!(
                        "engine diagnostic record {:?} field {name:?} is negative: {value}",
                        self.record
                    ));
                }
            }
        }
        None
    }
}

impl Assertion {
    fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Counter(assertion) => assertion.validate(),
            Self::Record(assertion) => assertion.validate(),
        }
    }

    fn violation(&self, counters: &Counters, stderr: &[u8]) -> Option<String> {
        match self {
            Self::Counter(assertion) => assertion.violation(counters),
            Self::Record(assertion) => assertion.violation(stderr),
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
            add_fields(&mut counters, fields);
        }
        Self(counters)
    }

    fn get(&self, counter: &str) -> Option<u64> {
        self.0.get(counter).copied()
    }

    /// Named so a missing counter reports what the engine did emit, including nothing at all.
    fn summary(&self) -> String {
        if self.0.is_empty() {
            return "none (the engine emitted no execution diagnostic record)".to_owned();
        }
        self.0
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn add_fields(counters: &mut BTreeMap<String, u64>, fields: &str) {
    for (name, value) in fields.split_whitespace().filter_map(|field| field.split_once('=')) {
        let Ok(value) = value.parse::<u64>() else { continue };
        *counters.entry(name.to_owned()).or_default() += value;
    }
}

fn record_fields(record: &str) -> Option<BTreeMap<&str, &str>> {
    let mut fields = BTreeMap::new();
    for field in record.split_whitespace() {
        let (name, value) = field.split_once('=')?;
        if name.is_empty() || value.is_empty() || fields.insert(name, value).is_some() {
            return None;
        }
    }
    Some(fields)
}

/// The first unmet assertion, or none. Absent diagnostics violate every assertion rather than
/// skipping it, because silently passing on missing data is the gap this exists to close.
pub(crate) fn violation(assertions: &[Assertion], stderr: &[u8]) -> Option<String> {
    if assertions.is_empty() {
        return None;
    }
    let counters = Counters::parse(stderr);
    assertions
        .iter()
        .find_map(|assertion| assertion.violation(&counters, stderr))
}

#[cfg(test)]
mod tests {
    use super::{Assertion, Counters, validate, violation};

    fn assertions(yaml: &str) -> Vec<Assertion> {
        serde_yaml::from_str(yaml).unwrap()
    }

    const REPORT: &[u8] = b"hl-native: crossings=3 translations=7 hits=9 fallbacks=0 sites=2 services=1\n\
                            hl-native-detail: fills=16 completed=58122035 a64_dirty_overflow=21\n";

    #[test]
    fn a_counter_above_its_bound_passes_and_one_at_it_fails() {
        assert!(violation(&assertions("- { counter: crossings, greater-than: 0 }"), REPORT).is_none());
        assert!(
            violation(&assertions("- { counter: crossings, greater-than: 3 }"), REPORT)
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
        let report = b"hl-native: crossings=0 translations=0 hits=0 fallbacks=0 sites=0 services=0\n";
        assert!(
            violation(&assertions("- { counter: crossings, greater-than: 0 }"), report)
                .unwrap()
                .contains("is 0")
        );
    }

    #[test]
    fn absent_diagnostics_fail_rather_than_skip() {
        let message = violation(&assertions("- { counter: crossings, greater-than: 0 }"), b"").unwrap();
        assert!(message.contains("never emitted"), "{message}");
        assert!(message.contains("no execution diagnostic record"), "{message}");
    }

    const FORK_FAILURE: &[u8] = b"hl-fork-failure: stage=pids-limit result_errno=11 host_snapshot_status=1 \
                                  host_threads=03 host_children=0 local_tasks=1 pids_total=1 pids_max=1 \
                                  open_fds=12 nofile_cur=1024 nofile_max=4096 nproc_cur=2048 nproc_max=4096\n";

    const FORK_ASSERTION: &str = "- record: 'hl-fork-failure:'\n  count: 1\n  fields:\n    stage: pids-limit\n    result_errno: '11'\n    host_snapshot_status: '1'\n  nonnegative: [host_threads, host_children, local_tasks, pids_total, pids_max, open_fds, nofile_cur, nofile_max, nproc_cur, nproc_max]\n";

    #[test]
    fn structured_failure_record_requires_one_parseable_real_host_snapshot() {
        assert!(violation(&assertions(FORK_ASSERTION), FORK_FAILURE).is_none());

        let missing = violation(&assertions(FORK_ASSERTION), b"").unwrap();
        assert!(missing.contains("appeared 0 times"), "{missing}");

        let duplicate = [FORK_FAILURE, FORK_FAILURE].concat();
        let duplicate = violation(&assertions(FORK_ASSERTION), &duplicate).unwrap();
        assert!(duplicate.contains("appeared 2 times"), "{duplicate}");

        let negative = FORK_FAILURE
            .windows(b"host_threads=03".len())
            .position(|window| window == b"host_threads=03")
            .expect("fixture field");
        let mut negative_report = FORK_FAILURE.to_vec();
        negative_report[negative + "host_threads=".len()] = b'-';
        let negative = violation(&assertions(FORK_ASSERTION), &negative_report).unwrap();
        assert!(negative.contains("is negative"), "{negative}");
    }

    #[test]
    fn retained_c_completion_satisfies_the_execution_floor() {
        let report = b"[prof] crossings=1 translations=9 syscalls=2 ibtc_miss=0\n";
        assert!(violation(&assertions("- { counter: crossings, greater-than: 0 }"), report).is_none());
        assert_eq!(Counters::parse(report).get("crossings"), Some(1));
        assert_eq!(super::digest(report), "c crossings=1 translations=9");
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
        let report = b"hl-native: crossings=1 translations=2\nhl-native: crossings=4 translations=1\n";
        assert_eq!(Counters::parse(report).get("crossings"), Some(5));
        assert_eq!(Counters::parse(report).get("translations"), Some(3));
    }

    #[test]
    fn histogram_lines_never_contribute_counters() {
        let report = b"hl-native-fallback-pc: pc=0x4000 count=12\nhl-native-suppressed-entry: pc=0x8 refused=3\n";
        assert_eq!(Counters::parse(report).get("count"), None);
        assert_eq!(Counters::parse(report).get("refused"), None);
    }

    #[test]
    fn the_digest_carries_counters_and_the_pcs_that_identify_the_translated_body() {
        let report = b"hl-native: crossings=1 translations=1 hits=2 fallbacks=3 sites=1 services=0\n\
                       hl-native-entry: probes=9 entries=1 declined_executable=0 declined_suppressed=8 declined_cold=0 declined_other=0\n\
                       hl-interp: instructions=4000 blocks=70 slices=3\n\
                       hl-native-fallback-pc: pc=0x426a1c count=12\n\
                       hl-native-suppressed-entry: pc=0x8 refused=3\n";
        let digest = super::digest(report);
        assert!(
            digest.starts_with("native crossings=1 translations=1 hits=2 fallbacks=3 sites=1 services=0"),
            "{digest}"
        );
        assert!(digest.contains("instructions=4000"), "{digest}");
        assert!(digest.contains("fallback_pc=0x426a1c:12"), "{digest}");
        assert!(digest.contains("suppressed_pc=0x8:3"), "{digest}");
        // The counters that carry no information stay out of the row.
        assert!(!digest.contains("ibtc_"), "{digest}");
        assert!(!digest.contains("a64_slim_exits"), "{digest}");
    }

    #[test]
    fn admission_digest_retains_matching_arm64_and_x86_guard_evidence() {
        let report = b"hl-native-detail: a64_guard_fast=11 a64_guard_full=12 a64_dirty_committed=13 \
                       x86_guard_fast=21 x86_guard_full=22 x86_dirty_committed=23\n";
        let digest = super::digest(report);
        for field in [
            "a64_guard_fast=11",
            "a64_guard_full=12",
            "a64_dirty_committed=13",
            "x86_guard_fast=21",
            "x86_guard_full=22",
            "x86_dirty_committed=23",
        ] {
            assert!(digest.contains(field), "missing {field} from {digest}");
        }
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
        assert!(
            full < base * 24,
            "digest scaled {full:?} against {base:?} for 8x the input"
        );
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
        assert!(validate(assertions("- { counter: crossings }"), true).is_err());
        assert!(validate(assertions("- { counter: crossings, equals: 1, greater-than: 0 }"), true).is_err());
        assert!(validate(assertions("- { counter: \" \", equals: 1 }"), true).is_err());
        assert!(validate(assertions("- { counter: crossings, equals: 1 }"), true).is_ok());
        assert!(validate(assertions("- { record: '', count: 1 }"), true).is_err());
        assert!(validate(assertions("- { record: event, count: 0 }"), true).is_err());
        assert!(
            validate(
                assertions("- { record: event, count: 1, nonnegative: ['bad=name'] }"),
                true
            )
            .is_err()
        );
    }

    #[test]
    fn asserting_on_counters_an_app_never_asks_for_is_a_load_error() {
        let error = validate(assertions("- { counter: crossings, greater-than: 0 }"), false).unwrap_err();
        assert!(error.to_string().contains("diagnostics: true"), "{error}");
        assert!(validate(Vec::new(), false).is_ok());
    }
}
