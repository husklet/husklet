use super::WorkKey;
use crate::{
    journal::{self, Attempt, Require as _, Schema},
    suite::{Error, Target},
};
use std::collections::BTreeSet;

pub(super) type Ledger = journal::Ledger<Bench>;

const OUTPUT_LIMIT: usize = 1024 * 1024;

#[derive(Clone)]
pub(super) struct Row {
    pub attempt: Attempt<WorkKey>,
    pub output: String,
}

/// The result schema of a benchmark run, whose keys carry the provenance they measured.
pub(super) struct Bench;

impl Bench {
    /// Hexadecimal so multi-line writer output survives a single tab-separated field.
    fn encode(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut result = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            result.push(HEX[(byte >> 4) as usize] as char);
            result.push(HEX[(byte & 0x0f) as usize] as char);
        }
        result
    }

    fn decode(value: &str) -> Result<String, Error> {
        value
            .len()
            .is_multiple_of(2)
            .require("invalid benchmark result encoding")?;
        let bytes = value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| Ok((Self::digit(pair[0])? << 4) | Self::digit(pair[1])?))
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(String::from_utf8(bytes)?)
    }

    fn digit(value: u8) -> Result<u8, Error> {
        match value {
            b'0'..=b'9' => Ok(value - b'0'),
            b'a'..=b'f' => Ok(value - b'a' + 10),
            _ => Err("invalid benchmark result encoding".into()),
        }
    }
}

impl Schema for Bench {
    type Key = WorkKey;
    type Row = Row;

    const KIND: &'static str = "benchmark";
    const HEADER: &'static str = "id\ttarget\tprovenance\tstatus\telapsed_ms\toutput\n";
    const ROW_LIMIT: usize = OUTPUT_LIMIT * 2 + 4096;
    const FIELDS: usize = 6;

    fn key(row: &Row) -> &WorkKey {
        &row.attempt.key
    }

    fn format(row: &Row) -> Result<String, Error> {
        (!row.attempt.key.id.contains(['\t', '\n']) && !row.attempt.key.provenance.contains(['\t', '\n']))
            .require("benchmark result contains an unsafe delimiter")?;
        (row.output.len() <= OUTPUT_LIMIT).require("benchmark result output exceeds its byte bound")?;
        let text = format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            row.attempt.key.id,
            row.attempt.key.target.name(),
            row.attempt.key.provenance,
            row.attempt.status,
            row.attempt.elapsed_ms,
            Self::encode(row.output.as_bytes())
        );
        (text.len() <= Self::ROW_LIMIT).require("benchmark result row exceeds its byte bound")?;
        Ok(text)
    }

    fn parse(fields: &[&str], keys: &BTreeSet<WorkKey>) -> Result<Option<Row>, Error> {
        let key = WorkKey {
            id: fields[0].to_owned(),
            target: Target::named(fields[1]).ok_or("invalid benchmark resume target")?,
            provenance: fields[2].to_owned(),
        };
        let status = match fields[3] {
            "pass" => "pass",
            "fail" => "fail",
            _ => return Err("invalid benchmark resume status".into()),
        };
        let elapsed_ms = fields[4].parse()?;
        let output = Self::decode(fields[5])?;
        if !keys.contains(&key) {
            // The same work under new provenance supersedes this row; anything else is stale.
            keys.iter()
                .any(|candidate| candidate.id == key.id && candidate.target == key.target)
                .require("stale benchmark resume row")?;
            return Ok(None);
        }
        Ok(Some(Row {
            attempt: Attempt {
                key,
                status,
                elapsed_ms,
            },
            output,
        }))
    }
}
#[cfg(test)]
mod tests {
    use super::{Attempt, Ledger, OUTPUT_LIMIT, Row, WorkKey};
    use crate::suite::Target;
    use std::{collections::BTreeSet, io::Write};

    fn key(id: &str) -> WorkKey {
        WorkKey {
            id: id.to_owned(),
            target: Target::Arm64,
            provenance: "provenance-a".to_owned(),
        }
    }

    #[test]
    fn durable_rows_resume_and_finish_in_key_order() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("results.tsv");
        let keys = BTreeSet::from([key("bench/a"), key("bench/b")]);
        let opened = Ledger::open(&report, "stamp", &keys, false).unwrap();
        opened
            .ledger
            .record(Row {
                attempt: Attempt {
                    key: key("bench/b"),
                    status: "pass",
                    elapsed_ms: 2,
                },
                output: "PASS bench/b\nPHASE bench/b".to_owned(),
            })
            .unwrap();
        drop(opened);
        let resumed = Ledger::open(&report, "stamp", &keys, true).unwrap();
        assert_eq!(resumed.prior[&key("bench/b")].output, "PASS bench/b\nPHASE bench/b");
        resumed
            .ledger
            .record(Row {
                attempt: Attempt {
                    key: key("bench/a"),
                    status: "fail",
                    elapsed_ms: 1,
                },
                output: "FAIL bench/a".to_owned(),
            })
            .unwrap();
        resumed.ledger.finish().unwrap();
        let text = std::fs::read_to_string(report).unwrap();
        assert!(text.find("bench/a").unwrap() < text.find("bench/b").unwrap());
    }

    #[test]
    fn torn_tail_is_dropped_and_stale_stamp_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("results.tsv");
        let keys = BTreeSet::from([key("bench/a")]);
        let opened = Ledger::open(&report, "stamp", &keys, false).unwrap();
        drop(opened);
        let partial = report.with_extension("partial.tsv");
        let mut file = std::fs::OpenOptions::new().append(true).open(&partial).unwrap();
        file.write_all(b"bench/a\tarm").unwrap();
        file.sync_data().unwrap();
        drop(file);
        assert!(Ledger::open(&report, "stamp", &keys, true).is_ok());
        assert!(Ledger::open(&report, "changed", &keys, true).is_err());
    }

    #[test]
    fn maximum_writer_output_round_trips_through_resume() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("results.tsv");
        let keys = BTreeSet::from([key("bench/boundary")]);
        let opened = Ledger::open(&report, "stamp", &keys, false).unwrap();
        opened
            .ledger
            .record(Row {
                attempt: Attempt {
                    key: key("bench/boundary"),
                    status: "pass",
                    elapsed_ms: 1,
                },
                output: "x".repeat(OUTPUT_LIMIT),
            })
            .unwrap();
        drop(opened);
        let resumed = Ledger::open(&report, "stamp", &keys, true).unwrap();
        assert_eq!(resumed.prior[&key("bench/boundary")].output.len(), OUTPUT_LIMIT);
    }

    #[test]
    fn resume_rejects_a_row_from_different_provenance() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("results.tsv");
        let original = key("bench/a");
        let opened = Ledger::open(&report, "stamp", &BTreeSet::from([original.clone()]), false).unwrap();
        opened
            .ledger
            .record(Row {
                attempt: Attempt {
                    key: original,
                    status: "pass",
                    elapsed_ms: 1,
                },
                output: "PASS bench/a".into(),
            })
            .unwrap();
        drop(opened);
        let mut changed = key("bench/a");
        changed.provenance = "provenance-b".into();
        let resumed = Ledger::open(&report, "stamp", &BTreeSet::from([changed]), true).unwrap();
        assert!(resumed.prior.is_empty());
    }

    #[test]
    fn measurement_ledger_refuses_reused_and_vacuous_resume_paths() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("results.tsv");
        let keys = BTreeSet::from([key("bench/a")]);

        assert!(Ledger::open_unique(&report, "stamp", &keys, true).is_err());
        let opened = Ledger::open_unique(&report, "stamp", &keys, false).unwrap();
        drop(opened);
        assert!(Ledger::open_unique(&report, "stamp", &keys, false).is_err());
        assert!(Ledger::open_unique(&report, "stamp", &keys, true).is_ok());
    }
}
