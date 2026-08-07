use super::WorkKey;
use super::diagnostic::BoundedDiagnostic as _;
use crate::{
    journal::{self, Attempt, Require as _, Schema},
    suite::{Error, Target},
};
use std::collections::BTreeSet;

pub(super) type Ledger = journal::Ledger<Runtime>;

pub(super) const PASS: &str = "pass";
pub(super) const FAIL: &str = "fail";
/// A case the sweep never attempted: deliberately inactive, or lost to an abort.
pub(super) const NOT_RUN: &str = "NOT_RUN";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Row {
    pub attempt: Attempt<WorkKey>,
    /// Host load when the row was recorded, so a contended run is distinguishable from a real timeout.
    pub host_load: String,
    pub diagnostic: String,
}

/// The result schema of a runtime compatibility run.
pub(super) struct Runtime;

impl Schema for Runtime {
    type Key = WorkKey;
    type Row = Row;

    const KIND: &'static str = "runtime";
    const HEADER: &'static str = "id\ttarget\tprofile\tstatus\telapsed_ms\thost_load\tdiagnostic\n";
    const ROW_LIMIT: usize = 16 * 1024;
    const FIELDS: usize = 7;

    fn key(row: &Row) -> &WorkKey {
        &row.attempt.key
    }

    /// Never fails on diagnostic shape: a truncated row is worth more than an aborted sweep.
    fn format(row: &Row) -> Result<String, Error> {
        (!row.attempt.key.id.contains(['\t', '\n'])).require("runtime result contains an unsafe delimiter")?;
        let load = super::load::sanitize(&row.host_load);
        let prefix = format!(
            "{}\t{}\t{}\t{}\t{}\t{load}\t",
            row.attempt.key.id,
            row.attempt.key.target.name(),
            super::profile::PROFILE,
            row.attempt.status,
            row.attempt.elapsed_ms
        );
        let diagnostic = row.diagnostic.replace(['\t', '\n'], " ");
        let room = Self::ROW_LIMIT.saturating_sub(prefix.len() + 1);
        (room > 64).require("runtime result key exceeds its byte bound")?;
        Ok(format!("{prefix}{}\n", diagnostic.bounded_to(room)))
    }

    fn parse(fields: &[&str], keys: &BTreeSet<WorkKey>) -> Result<Option<Row>, Error> {
        let key = WorkKey {
            id: fields[0].to_owned(),
            target: Target::named(fields[1]).ok_or("invalid runtime resume target")?,
        };
        keys.contains(&key).require("stale runtime resume row")?;
        (fields[2] == super::profile::PROFILE).require("runtime resume row measured another engine profile")?;
        let status = match fields[3] {
            "pass" => PASS,
            "fail" => FAIL,
            NOT_RUN => NOT_RUN,
            _ => return Err("invalid runtime resume status".into()),
        };
        Ok(Some(Row {
            attempt: Attempt {
                key,
                status,
                elapsed_ms: fields[4].parse()?,
            },
            host_load: fields[5].to_owned(),
            diagnostic: fields[6].to_owned(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{Attempt, Ledger, Row, WorkKey};
    use crate::suite::Target;
    use std::collections::BTreeSet;
    use std::io::Write;

    fn key(id: &str) -> WorkKey {
        WorkKey {
            id: id.to_owned(),
            target: Target::Arm64,
        }
    }

    #[test]
    fn durable_rows_resume_and_finish_in_key_order() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("results.tsv");
        let keys = BTreeSet::from([key("runtime/a"), key("runtime/b")]);
        let opened = Ledger::open(&report, "stamp", &keys, false).unwrap();
        opened
            .ledger
            .record(Row {
                attempt: Attempt {
                    key: key("runtime/b"),
                    status: "pass",
                    elapsed_ms: 2,
                },
                host_load: "0.20/8".to_owned(),
                diagnostic: String::new(),
            })
            .unwrap();
        drop(opened);
        let resumed = Ledger::open(&report, "stamp", &keys, true).unwrap();
        assert_eq!(resumed.prior.len(), 1);
        resumed
            .ledger
            .record(Row {
                attempt: Attempt {
                    key: key("runtime/a"),
                    status: "pass",
                    elapsed_ms: 1,
                },
                host_load: "0.10/8".to_owned(),
                diagnostic: String::new(),
            })
            .unwrap();
        resumed.ledger.finish().unwrap();
        let text = std::fs::read_to_string(report).unwrap();
        assert!(text.find("runtime/a").unwrap() < text.find("runtime/b").unwrap());
    }

    #[test]
    fn an_oversized_diagnostic_is_truncated_rather_than_rejected() {
        use crate::journal::Schema as _;

        let row = Row {
            attempt: Attempt {
                key: key("runtime/loud"),
                status: super::FAIL,
                elapsed_ms: 1,
            },
            host_load: "9.00/8".to_owned(),
            diagnostic: "x\ty\n".repeat(1 << 16),
        };
        let text = super::Runtime::format(&row).unwrap();
        assert!(text.len() <= super::Runtime::ROW_LIMIT, "{}", text.len());
        assert_eq!(text.matches('\t').count(), 6);
        assert!(text.contains(super::super::profile::PROFILE), "{text}");
        assert!(text.ends_with("truncated]\n"), "{text}");
    }

    #[test]
    fn not_run_rows_round_trip_through_resume() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("results.tsv");
        let keys = BTreeSet::from([key("runtime/a")]);
        let opened = Ledger::open(&report, "stamp", &keys, false).unwrap();
        opened
            .ledger
            .record(Row {
                attempt: Attempt {
                    key: key("runtime/a"),
                    status: super::NOT_RUN,
                    elapsed_ms: 0,
                },
                host_load: super::super::load::unmeasured(),
                diagnostic: "BROKEN: retained".to_owned(),
            })
            .unwrap();
        drop(opened);
        let resumed = Ledger::open(&report, "stamp", &keys, true).unwrap();
        assert_eq!(resumed.prior[&key("runtime/a")].attempt.status, super::NOT_RUN);
        assert_eq!(resumed.ledger.planned(), &keys);
    }

    #[test]
    fn torn_tail_is_dropped_and_stale_stamp_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("results.tsv");
        let keys = BTreeSet::from([key("runtime/a")]);
        let opened = Ledger::open(&report, "stamp", &keys, false).unwrap();
        drop(opened);
        let partial = report.with_extension("partial.tsv");
        let mut file = std::fs::OpenOptions::new().append(true).open(&partial).unwrap();
        file.write_all(b"runtime/a\tarm").unwrap();
        file.sync_data().unwrap();
        drop(file);
        assert!(Ledger::open(&report, "stamp", &keys, true).is_ok());
        assert!(Ledger::open(&report, "changed", &keys, true).is_err());
    }
}
