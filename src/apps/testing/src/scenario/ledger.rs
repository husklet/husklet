use super::{scheduler::WorkKey, terminal::Metric};
use crate::{
    journal::{self, Require as _, Schema},
    suite::{Error, Target},
};
use std::collections::BTreeSet;

pub(super) type Ledger = journal::Ledger<Scenario>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Row {
    pub key: WorkKey,
    pub status: &'static str,
    pub elapsed_ms: u64,
    pub timing: super::execution::PhaseTiming,
    pub diagnostic: String,
}

/// The result schema of a scenario run, including its per-phase timing.
pub(super) struct Scenario;

impl Scenario {
    fn format_metrics(metrics: &[Metric]) -> String {
        metrics
            .iter()
            .enumerate()
            .map(|(index, metric)| {
                format!(
                    "{index}:{}:{}:{}:{}:{}",
                    metric.operation,
                    metric.elapsed_us,
                    metric.bytes_written,
                    metric.bytes_read,
                    u8::from(metric.succeeded)
                )
            })
            .collect::<Vec<_>>()
            .join(";")
    }

    fn parse_metrics(value: &str) -> Result<Vec<Metric>, Error> {
        if value.is_empty() {
            return Ok(Vec::new());
        }
        value
            .split(';')
            .enumerate()
            .map(|(expected_index, value)| {
                let fields = value.split(':').collect::<Vec<_>>();
                (fields.len() == 6).require("invalid terminal metric")?;
                (fields[0].parse::<usize>()? == expected_index).require("unordered terminal metric")?;
                let operation = match fields[1] {
                    "write" => "write",
                    "resize" => "resize",
                    "close" => "close",
                    "await_output" => "await_output",
                    "reject_output" => "reject_output",
                    "drain" => "drain",
                    _ => return Err("invalid terminal metric operation".into()),
                };
                Ok(Metric {
                    operation,
                    elapsed_us: fields[2].parse()?,
                    bytes_written: fields[3].parse()?,
                    bytes_read: fields[4].parse()?,
                    succeeded: match fields[5] {
                        "0" => false,
                        "1" => true,
                        _ => return Err("invalid terminal metric outcome".into()),
                    },
                })
            })
            .collect()
    }
}

impl Schema for Scenario {
    type Key = WorkKey;
    type Row = Row;

    const KIND: &'static str = "scenario";
    const HEADER: &'static str = "id\ttarget\tsample\tstatus\telapsed_ms\tsetup_us\texecution_us\tpayload_us\tteardown_us\tterminal_steps\tdiagnostic\n";
    const ROW_LIMIT: usize = 16 * 1024;
    const FIELDS: usize = 11;

    fn key(row: &Row) -> &WorkKey {
        &row.key
    }

    fn format(row: &Row) -> Result<String, Error> {
        (!row.key.id.contains(['\t', '\n']) && !row.diagnostic.contains(['\t', '\n']))
            .require("scenario result contains an unsafe delimiter")?;
        let text = format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            row.key.id,
            row.key.target.name(),
            row.key.sample,
            row.status,
            row.elapsed_ms,
            row.timing.setup_us,
            row.timing.execution_us,
            row.timing
                .payload_us
                .map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
            row.timing.teardown_us,
            Self::format_metrics(&row.timing.terminal_steps),
            row.diagnostic
        );
        (text.len() <= Self::ROW_LIMIT).require("scenario result row exceeds its byte bound")?;
        Ok(text)
    }

    fn parse(fields: &[&str], keys: &BTreeSet<WorkKey>) -> Result<Option<Row>, Error> {
        let key = WorkKey {
            id: fields[0].to_owned(),
            target: Target::named(fields[1]).ok_or("invalid scenario resume target")?,
            sample: fields[2].parse()?,
        };
        keys.contains(&key).require("stale scenario resume row")?;
        let status = match fields[3] {
            "pass" => "pass",
            "xfail" => "xfail",
            "fail" => "fail",
            "xpass" => "xpass",
            "skip" => "skip",
            _ => return Err("invalid scenario resume status".into()),
        };
        Ok(Some(Row {
            key,
            status,
            elapsed_ms: fields[4].parse()?,
            timing: super::execution::PhaseTiming {
                setup_us: fields[5].parse()?,
                execution_us: fields[6].parse()?,
                payload_us: (fields[7] != "unavailable").then(|| fields[7].parse()).transpose()?,
                teardown_us: fields[8].parse()?,
                terminal_steps: Self::parse_metrics(fields[9])?,
            },
            diagnostic: fields[10].to_owned(),
        }))
    }
}
#[cfg(test)]
mod tests {
    use super::{Ledger, Row, WorkKey};
    use crate::scenario::execution::PhaseTiming;
    use crate::suite::Target;
    use std::collections::BTreeSet;
    use std::io::Write;

    fn key(id: &str) -> WorkKey {
        sample_key(id, 1)
    }

    fn sample_key(id: &str, sample: u16) -> WorkKey {
        WorkKey {
            id: id.to_owned(),
            target: Target::Arm64,
            sample,
        }
    }

    #[test]
    fn rows_are_durable_and_sorted() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("results.tsv");
        let keys = BTreeSet::from([key("scenario/a"), key("scenario/b")]);
        let opened = Ledger::open(&report, "stamp", &keys, false).unwrap();
        for id in ["scenario/b", "scenario/a"] {
            opened
                .ledger
                .record(Row {
                    key: key(id),
                    status: "pass",
                    elapsed_ms: 1,
                    timing: PhaseTiming {
                        setup_us: 2,
                        execution_us: 3,
                        payload_us: Some(1),
                        teardown_us: 4,
                        terminal_steps: vec![crate::scenario::terminal::Metric {
                            operation: "write",
                            elapsed_us: 5,
                            bytes_written: 3,
                            bytes_read: 0,
                            succeeded: true,
                        }],
                    },
                    diagnostic: String::new(),
                })
                .unwrap();
        }
        opened.ledger.finish().unwrap();
        let text = std::fs::read_to_string(report).unwrap();
        assert!(text.find("scenario/a").unwrap() < text.find("scenario/b").unwrap());
        assert!(text.contains("\t2\t3\t1\t4\t0:write:5:3:0:1\t"));
    }

    #[test]
    fn sample_rows_are_sorted_and_independently_resumable() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("results.tsv");
        let keys = BTreeSet::from([sample_key("scenario/a", 1), sample_key("scenario/a", 2)]);
        let opened = Ledger::open(&report, "stamp", &keys, false).unwrap();
        for sample in [2, 1] {
            opened
                .ledger
                .record(Row {
                    key: sample_key("scenario/a", sample),
                    status: if sample == 2 { "skip" } else { "pass" },
                    elapsed_ms: u64::from(sample),
                    timing: PhaseTiming::default(),
                    diagnostic: String::new(),
                })
                .unwrap();
        }
        drop(opened);
        let resumed = Ledger::open(&report, "stamp", &keys, true).unwrap();
        assert_eq!(resumed.prior.len(), 2);
        assert_eq!(resumed.prior[&sample_key("scenario/a", 1)].elapsed_ms, 1);
        assert_eq!(resumed.prior[&sample_key("scenario/a", 2)].elapsed_ms, 2);
        assert_eq!(resumed.prior[&sample_key("scenario/a", 2)].status, "skip");
    }

    #[test]
    fn resume_preserves_status_and_rejects_stale_input() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("results.tsv");
        let keys = BTreeSet::from([key("scenario/a")]);
        let opened = Ledger::open(&report, "stamp", &keys, false).unwrap();
        opened
            .ledger
            .record(Row {
                key: key("scenario/a"),
                status: "xfail",
                elapsed_ms: 3,
                timing: PhaseTiming::default(),
                diagnostic: "known gap".to_owned(),
            })
            .unwrap();
        drop(opened);
        let resumed = Ledger::open(&report, "stamp", &keys, true).unwrap();
        assert_eq!(resumed.prior[&key("scenario/a")].status, "xfail");
        drop(resumed);
        assert!(Ledger::open(&report, "changed", &keys, true).is_err());
    }

    #[test]
    fn resume_drops_a_torn_tail() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("results.tsv");
        let keys = BTreeSet::from([key("scenario/a")]);
        let opened = Ledger::open(&report, "stamp", &keys, false).unwrap();
        drop(opened);
        let partial = report.with_extension("partial.tsv");
        let mut file = std::fs::OpenOptions::new().append(true).open(&partial).unwrap();
        file.write_all(b"scenario/a\tarm").unwrap();
        file.sync_data().unwrap();
        drop(file);
        assert!(Ledger::open(&report, "stamp", &keys, true).is_ok());
    }
}
