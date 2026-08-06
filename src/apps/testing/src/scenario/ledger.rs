use super::scheduler::WorkKey;
use crate::suite::{Error, Target};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const HEADER: &str = "id\ttarget\tsample\tstatus\telapsed_ms\tsetup_us\texecution_us\tpayload_us\tteardown_us\tterminal_steps\tdiagnostic\n";
const ROW_LIMIT: usize = 16 * 1024;
const FILE_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Row {
    pub key: WorkKey,
    pub status: &'static str,
    pub elapsed_ms: u64,
    pub timing: super::execution::PhaseTiming,
    pub diagnostic: String,
}

pub(super) struct Ledger {
    report: PathBuf,
    partial: PathBuf,
    file: Mutex<Option<BufWriter<File>>>,
    rows: Mutex<BTreeMap<WorkKey, Row>>,
    _lock: hl_engine::native::FileLock,
}

pub(super) struct Opened {
    pub ledger: Ledger,
    pub prior: BTreeMap<WorkKey, Row>,
}

impl Ledger {
    pub fn open(report: &Path, stamp: &str, keys: &BTreeSet<WorkKey>, resume: bool) -> Result<Opened, Error> {
        let partial = report.with_extension("partial.tsv");
        if let Some(parent) = report.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock = hl_engine::native::FileLock::acquire(report.with_extension("lock"))?;
        let prior = if resume && partial.exists() {
            load(&partial, stamp, keys)?
        } else {
            initialize(&partial, stamp)?;
            BTreeMap::new()
        };
        let file = OpenOptions::new().append(true).open(&partial)?;
        Ok(Opened {
            ledger: Ledger {
                report: report.to_path_buf(),
                partial,
                file: Mutex::new(Some(BufWriter::new(file))),
                rows: Mutex::new(prior.clone()),
                _lock: lock,
            },
            prior,
        })
    }

    pub fn record(&self, row: Row) -> Result<(), Error> {
        let text = format_row(&row)?;
        let current = fs::metadata(&self.partial)?.len();
        if current
            .checked_add(text.len() as u64)
            .ok_or("scenario result file size overflow")?
            > FILE_LIMIT
        {
            return Err(format!("scenario result journal exceeded {FILE_LIMIT} bytes").into());
        }
        let mut guard = self.file.lock().map_err(|_| "scenario result journal lock poisoned")?;
        let file = guard.as_mut().ok_or("scenario result journal is finalized")?;
        file.write_all(text.as_bytes())?;
        file.flush()?;
        file.get_ref().sync_data()?;
        self.rows
            .lock()
            .map_err(|_| "scenario result rows lock poisoned")?
            .insert(row.key.clone(), row);
        Ok(())
    }

    pub fn finish(&self) -> Result<(), Error> {
        let mut journal = self.file.lock().map_err(|_| "scenario result journal lock poisoned")?;
        if let Some(mut file) = journal.take() {
            file.flush()?;
            file.get_ref().sync_data()?;
        }
        let temporary = self.report.with_extension("tmp");
        let rows = self.rows.lock().map_err(|_| "scenario result rows lock poisoned")?;
        let mut file = BufWriter::new(File::create(&temporary)?);
        file.write_all(HEADER.as_bytes())?;
        for row in rows.values() {
            file.write_all(format_row(row)?.as_bytes())?;
        }
        file.flush()?;
        file.get_ref().sync_data()?;
        drop(file);
        fs::rename(&temporary, &self.report)?;
        fs::remove_file(&self.partial)?;
        Ok(())
    }
}

fn initialize(path: &Path, stamp: &str) -> Result<(), Error> {
    let mut file = BufWriter::new(File::create(path)?);
    writeln!(file, "# scenario-run\t{stamp}")?;
    file.write_all(HEADER.as_bytes())?;
    file.flush()?;
    file.get_ref().sync_data()?;
    Ok(())
}

fn load(path: &Path, stamp: &str, keys: &BTreeSet<WorkKey>) -> Result<BTreeMap<WorkKey, Row>, Error> {
    let mut bytes = fs::read(path)?;
    require(
        bytes.len() as u64 <= FILE_LIMIT,
        "scenario result journal exceeds its byte bound",
    )?;
    if !bytes.ends_with(b"\n") {
        let complete = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        bytes.truncate(complete);
        let file = OpenOptions::new().write(true).open(path)?;
        file.set_len(complete as u64)?;
        file.sync_data()?;
    }
    let text = std::str::from_utf8(&bytes)?;
    let mut lines = text.lines();
    require(
        lines.next() == Some(format!("# scenario-run\t{stamp}").as_str()),
        "scenario resume input changed",
    )?;
    require(
        lines.next() == Some(HEADER.trim_end()),
        "scenario resume schema changed",
    )?;
    let mut rows = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        require(line.len() <= ROW_LIMIT, "scenario resume row exceeds its byte bound")?;
        let fields = line.split('\t').collect::<Vec<_>>();
        require(fields.len() == 11, "invalid scenario resume row")?;
        let target = match fields[1] {
            "arm64" => Target::Arm64,
            "amd64" => Target::Amd64,
            _ => return Err("invalid scenario resume target".into()),
        };
        let key = WorkKey {
            id: fields[0].to_owned(),
            target,
            sample: fields[2].parse()?,
        };
        require(keys.contains(&key), "stale scenario resume row")?;
        require(!rows.contains_key(&key), "duplicate scenario resume row")?;
        let status = match fields[3] {
            "pass" => "pass",
            "xfail" => "xfail",
            "fail" => "fail",
            "xpass" => "xpass",
            "skip" => "skip",
            _ => return Err("invalid scenario resume status".into()),
        };
        rows.insert(
            key.clone(),
            Row {
                key,
                status,
                elapsed_ms: fields[4].parse()?,
                timing: super::execution::PhaseTiming {
                    setup_us: fields[5].parse()?,
                    execution_us: fields[6].parse()?,
                    payload_us: (fields[7] != "unavailable").then(|| fields[7].parse()).transpose()?,
                    teardown_us: fields[8].parse()?,
                    terminal_steps: parse_terminal_metrics(fields[9])?,
                },
                diagnostic: fields[10].to_owned(),
            },
        );
    }
    Ok(rows)
}

fn format_row(row: &Row) -> Result<String, Error> {
    require(
        !row.key.id.contains(['\t', '\n']) && !row.diagnostic.contains(['\t', '\n']),
        "scenario result contains an unsafe delimiter",
    )?;
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
        format_terminal_metrics(&row.timing.terminal_steps),
        row.diagnostic
    );
    require(text.len() <= ROW_LIMIT, "scenario result row exceeds its byte bound")?;
    Ok(text)
}

fn format_terminal_metrics(metrics: &[super::terminal::Metric]) -> String {
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

fn parse_terminal_metrics(value: &str) -> Result<Vec<super::terminal::Metric>, Error> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    value
        .split(';')
        .enumerate()
        .map(|(expected_index, value)| {
            let fields = value.split(':').collect::<Vec<_>>();
            require(fields.len() == 6, "invalid terminal metric")?;
            require(
                fields[0].parse::<usize>()? == expected_index,
                "unordered terminal metric",
            )?;
            let operation = match fields[1] {
                "write" => "write",
                "resize" => "resize",
                "close" => "close",
                "await_output" => "await_output",
                "reject_output" => "reject_output",
                "drain" => "drain",
                _ => return Err("invalid terminal metric operation".into()),
            };
            Ok(super::terminal::Metric {
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

fn require(condition: bool, message: &'static str) -> Result<(), Error> {
    condition.then_some(()).ok_or_else(|| message.into())
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
