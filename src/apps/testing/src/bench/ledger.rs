use super::WorkKey;
use crate::suite::{Error, Target};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

const HEADER: &str = "id\ttarget\tstatus\telapsed_ms\toutput\n";
const OUTPUT_LIMIT: usize = 1024 * 1024;
const ROW_LIMIT: usize = OUTPUT_LIMIT * 2 + 4096;
const FILE_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Clone)]
pub(super) struct Row {
    pub key: WorkKey,
    pub status: &'static str,
    pub elapsed_ms: u64,
    pub output: String,
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
        let lock = hl_engine::native::FileLock::acquire(&report.with_extension("lock"))?;
        let prior = if resume && partial.exists() {
            load(&partial, stamp, keys)?
        } else {
            initialize(&partial, stamp)?;
            BTreeMap::new()
        };
        let file = OpenOptions::new().append(true).open(&partial)?;
        Ok(Opened {
            ledger: Self {
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
        if fs::metadata(&self.partial)?
            .len()
            .checked_add(text.len() as u64)
            .ok_or("benchmark result file size overflow")?
            > FILE_LIMIT
        {
            return Err(format!("benchmark result journal exceeded {FILE_LIMIT} bytes").into());
        }
        let mut guard = self.file.lock().map_err(|_| "benchmark result journal lock poisoned")?;
        let file = guard.as_mut().ok_or("benchmark result journal is finalized")?;
        file.write_all(text.as_bytes())?;
        file.flush()?;
        file.get_ref().sync_data()?;
        self.rows
            .lock()
            .map_err(|_| "benchmark result rows lock poisoned")?
            .insert(row.key.clone(), row);
        Ok(())
    }

    pub fn finish(&self) -> Result<(), Error> {
        let mut journal = self.file.lock().map_err(|_| "benchmark result journal lock poisoned")?;
        if let Some(mut file) = journal.take() {
            file.flush()?;
            file.get_ref().sync_data()?;
        }
        let temporary = self.report.with_extension("tmp");
        let rows = self.rows.lock().map_err(|_| "benchmark result rows lock poisoned")?;
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
    writeln!(file, "# benchmark-run\t{stamp}")?;
    file.write_all(HEADER.as_bytes())?;
    file.flush()?;
    file.get_ref().sync_data()?;
    Ok(())
}

fn load(path: &Path, stamp: &str, keys: &BTreeSet<WorkKey>) -> Result<BTreeMap<WorkKey, Row>, Error> {
    let mut bytes = fs::read(path)?;
    if bytes.len() as u64 > FILE_LIMIT {
        return Err("benchmark result journal exceeds its byte bound".into());
    }
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
        lines.next() == Some(format!("# benchmark-run\t{stamp}").as_str()),
        "benchmark resume input changed",
    )?;
    require(
        lines.next() == Some(HEADER.trim_end()),
        "benchmark resume schema changed",
    )?;
    let mut rows = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        require(line.len() <= ROW_LIMIT, "benchmark resume row exceeds its byte bound")?;
        let fields = line.split('\t').collect::<Vec<_>>();
        require(fields.len() == 5, "invalid benchmark resume row")?;
        let target = match fields[1] {
            "arm64" => Target::Arm64,
            "amd64" => Target::Amd64,
            _ => return Err("invalid benchmark resume target".into()),
        };
        let key = WorkKey {
            id: fields[0].to_owned(),
            target,
        };
        require(keys.contains(&key), "stale benchmark resume row")?;
        require(!rows.contains_key(&key), "duplicate benchmark resume row")?;
        let status = match fields[2] {
            "pass" => "pass",
            "fail" => "fail",
            _ => return Err("invalid benchmark resume status".into()),
        };
        rows.insert(
            key.clone(),
            Row {
                key,
                status,
                elapsed_ms: fields[3].parse()?,
                output: decode(fields[4])?,
            },
        );
    }
    Ok(rows)
}

fn format_row(row: &Row) -> Result<String, Error> {
    require(
        !row.key.id.contains(['\t', '\n']),
        "benchmark result contains an unsafe delimiter",
    )?;
    require(
        row.output.len() <= OUTPUT_LIMIT,
        "benchmark result output exceeds its byte bound",
    )?;
    let text = format!(
        "{}\t{}\t{}\t{}\t{}\n",
        row.key.id,
        row.key.target.name(),
        row.status,
        row.elapsed_ms,
        encode(row.output.as_bytes())
    );
    require(text.len() <= ROW_LIMIT, "benchmark result row exceeds its byte bound")?;
    Ok(text)
}

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
    if !value.len().is_multiple_of(2) {
        return Err("invalid benchmark result encoding".into());
    }
    let bytes = value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = digit(pair[0])?;
            let low = digit(pair[1])?;
            Ok((high << 4) | low)
        })
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

fn require(condition: bool, message: &'static str) -> Result<(), Error> {
    condition.then_some(()).ok_or_else(|| message.into())
}

#[cfg(test)]
mod tests {
    use super::{Ledger, OUTPUT_LIMIT, Row, WorkKey};
    use crate::suite::Target;
    use std::{collections::BTreeSet, io::Write};

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
        let keys = BTreeSet::from([key("bench/a"), key("bench/b")]);
        let opened = Ledger::open(&report, "stamp", &keys, false).unwrap();
        opened
            .ledger
            .record(Row {
                key: key("bench/b"),
                status: "pass",
                elapsed_ms: 2,
                output: "PASS bench/b\nPHASE bench/b".to_owned(),
            })
            .unwrap();
        drop(opened);
        let resumed = Ledger::open(&report, "stamp", &keys, true).unwrap();
        assert_eq!(resumed.prior[&key("bench/b")].output, "PASS bench/b\nPHASE bench/b");
        resumed
            .ledger
            .record(Row {
                key: key("bench/a"),
                status: "fail",
                elapsed_ms: 1,
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
                key: key("bench/boundary"),
                status: "pass",
                elapsed_ms: 1,
                output: "x".repeat(OUTPUT_LIMIT),
            })
            .unwrap();
        drop(opened);
        let resumed = Ledger::open(&report, "stamp", &keys, true).unwrap();
        assert_eq!(resumed.prior[&key("bench/boundary")].output.len(), OUTPUT_LIMIT);
    }
}
