use super::WorkKey;
use crate::{
    journal::{self, Require as _, Schema},
    suite::{Error, Target},
};
use std::collections::BTreeSet;

pub(super) type Ledger = journal::Ledger<Runtime>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Row {
    pub key: WorkKey,
    pub status: &'static str,
    pub elapsed_ms: u64,
    pub diagnostic: String,
}

/// The result schema of a runtime compatibility run.
pub(super) struct Runtime;

impl Schema for Runtime {
    type Key = WorkKey;
    type Row = Row;

    const KIND: &'static str = "runtime";
    const HEADER: &'static str = "id\ttarget\tstatus\telapsed_ms\tdiagnostic\n";
    const ROW_LIMIT: usize = 16 * 1024;
    const FIELDS: usize = 5;

    fn key(row: &Row) -> &WorkKey {
        &row.key
    }

    fn format(row: &Row) -> Result<String, Error> {
        (!row.key.id.contains(['\t', '\n']) && !row.diagnostic.contains(['\t', '\n']))
            .require("runtime result contains an unsafe delimiter")?;
        let text = format!(
            "{}\t{}\t{}\t{}\t{}\n",
            row.key.id,
            row.key.target.name(),
            row.status,
            row.elapsed_ms,
            row.diagnostic
        );
        (text.len() <= Self::ROW_LIMIT).require("runtime result row exceeds its byte bound")?;
        Ok(text)
    }

    fn parse(fields: &[&str], keys: &BTreeSet<WorkKey>) -> Result<Option<Row>, Error> {
        let key = WorkKey {
            id: fields[0].to_owned(),
            target: Target::named(fields[1]).ok_or("invalid runtime resume target")?,
        };
        keys.contains(&key).require("stale runtime resume row")?;
        let status = match fields[2] {
            "pass" => "pass",
            "fail" => "fail",
            _ => return Err("invalid runtime resume status".into()),
        };
        Ok(Some(Row {
            key,
            status,
            elapsed_ms: fields[3].parse()?,
            diagnostic: fields[4].to_owned(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{Ledger, Row, WorkKey};
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
                key: key("runtime/b"),
                status: "pass",
                elapsed_ms: 2,
                diagnostic: String::new(),
            })
            .unwrap();
        drop(opened);
        let resumed = Ledger::open(&report, "stamp", &keys, true).unwrap();
        assert_eq!(resumed.prior.len(), 1);
        resumed
            .ledger
            .record(Row {
                key: key("runtime/a"),
                status: "pass",
                elapsed_ms: 1,
                diagnostic: String::new(),
            })
            .unwrap();
        resumed.ledger.finish().unwrap();
        let text = std::fs::read_to_string(report).unwrap();
        assert!(text.find("runtime/a").unwrap() < text.find("runtime/b").unwrap());
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
