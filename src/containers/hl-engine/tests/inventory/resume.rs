use super::{Case, REPORT_HEADER, ResultRow, format_row};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Error, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub(super) struct Ledger {
    path: PathBuf,
    file: Mutex<Option<BufWriter<File>>>,
    _lock: hl_engine::native::FileLock,
}

pub(super) struct Run {
    pub pending: Vec<Case>,
    pub prior: Vec<ResultRow>,
    pub ledger: Ledger,
}

impl Ledger {
    pub fn record(&self, row: &ResultRow) -> std::io::Result<()> {
        let mut binding = self
            .file
            .lock()
            .map_err(|_| Error::other("compatibility ledger lock poisoned"))?;
        let file = binding
            .as_mut()
            .ok_or_else(|| Error::other("compatibility ledger is finalized"))?;
        file.write_all(format_row(row).as_bytes())?;
        file.flush()?;
        file.get_ref().sync_data()
    }

    pub fn finish(&self) -> std::io::Result<()> {
        let mut binding = self
            .file
            .lock()
            .map_err(|_| Error::other("compatibility ledger lock poisoned"))?;
        let mut file = binding
            .take()
            .ok_or_else(|| Error::other("compatibility ledger is finalized"))?;
        file.flush()?;
        file.get_ref().sync_data()?;
        drop(file);
        fs::remove_file(&self.path)
    }
}

pub(super) fn open(report: &Path, stamp: &str, cases: Vec<Case>, enabled: bool) -> std::io::Result<Run> {
    let path = report.with_extension("partial.tsv");
    let lock_path = report.with_extension("lock");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock = hl_engine::native::FileLock::acquire(&lock_path)?;
    let mut cases = cases
        .into_iter()
        .map(|case| (case.key(), case))
        .collect::<BTreeMap<_, _>>();
    let prior = if enabled && path.exists() {
        load(&path, stamp, &mut cases)?
    } else {
        let mut file = BufWriter::new(File::create(&path)?);
        writeln!(file, "# compatibility-run\t{stamp}")?;
        file.write_all(REPORT_HEADER.as_bytes())?;
        file.flush()?;
        file.get_ref().sync_data()?;
        Vec::new()
    };
    let file = OpenOptions::new().append(true).open(&path)?;
    Ok(Run {
        pending: cases.into_values().collect(),
        prior,
        ledger: Ledger {
            path,
            file: Mutex::new(Some(BufWriter::new(file))),
            _lock: lock,
        },
    })
}

fn load(
    path: &Path,
    stamp: &str,
    cases: &mut BTreeMap<(String, String, String), Case>,
) -> std::io::Result<Vec<ResultRow>> {
    let mut bytes = fs::read(path)?;
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
    let text = std::str::from_utf8(&bytes).map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
    let mut lines = text.lines();
    let expected = format!("# compatibility-run\t{stamp}");
    require(
        lines.next() == Some(expected.as_str()),
        "compatibility resume input changed",
    )?;
    require(
        lines.next() == Some(REPORT_HEADER.trim_end()),
        "compatibility resume schema changed",
    )?;
    let mut rows = Vec::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let fields = line.split('\t').collect::<Vec<_>>();
        require(fields.len() == 8, "invalid compatibility resume row")?;
        let key = (fields[0].to_owned(), fields[1].to_owned(), fields[2].to_owned());
        let case = cases
            .remove(&key)
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "stale or duplicate compatibility resume row"))?;
        require(
            fields[5] == case.dependencies,
            "compatibility dependency metadata changed",
        )?;
        require(
            matches!(fields[3], "pass" | "fail" | "skip"),
            "invalid compatibility status",
        )?;
        rows.push(ResultRow {
            case,
            status: match fields[3] {
                "pass" => "pass",
                "fail" => "fail",
                "skip" => "skip",
                _ => unreachable!(),
            },
            exit: fields[4].to_owned(),
            mismatch: fields[6].to_owned(),
            diagnostic: fields[7].to_owned(),
        });
    }
    Ok(rows)
}

fn require(condition: bool, message: &'static str) -> std::io::Result<()> {
    condition
        .then_some(())
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, message))
}

#[cfg(test)]
mod test {
    use super::open;
    use crate::{Case, SCRATCH, row};
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;

    fn case() -> Case {
        Case {
            suite: "suite".into(),
            name: "case".into(),
            isa: "aarch64".into(),
            artifact: PathBuf::from("artifact"),
            exit: 0,
            stdout: None,
            stderr: None,
            timeout: 10,
            environment: "-".into(),
            dependencies: "linux-libc".into(),
            skip: None,
            fixture: "executable".into(),
            arguments: "-".into(),
            side_files: "-".into(),
            rootfs: "-".into(),
            guest_executable: "/artifact".into(),
        }
    }

    #[test]
    fn completed_row_resumes() {
        let root = std::env::temp_dir().join(format!(
            "hl-resume-{}-{}",
            std::process::id(),
            SCRATCH.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&root).unwrap();
        let report = root.join("result.tsv");
        let first = open(&report, "stamp", vec![case()], false).unwrap();
        first.ledger.record(&row(case(), "pass", "0", "ok", "-")).unwrap();
        drop(first);
        let resumed = open(&report, "stamp", vec![case()], true).unwrap();
        assert!(resumed.pending.is_empty());
        assert_eq!(resumed.prior.len(), 1);
        resumed.ledger.finish().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn torn_tail_drops() {
        let root = std::env::temp_dir().join(format!(
            "hl-resume-torn-{}-{}",
            std::process::id(),
            SCRATCH.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&root).unwrap();
        let report = root.join("result.tsv");
        let first = open(&report, "stamp", vec![case()], false).unwrap();
        first.ledger.record(&row(case(), "pass", "0", "ok", "-")).unwrap();
        drop(first);
        let partial = report.with_extension("partial.tsv");
        let mut file = std::fs::OpenOptions::new().append(true).open(&partial).unwrap();
        file.write_all(b"suite\ttorn").unwrap();
        file.sync_data().unwrap();
        drop(file);
        let resumed = open(&report, "stamp", vec![case()], true).unwrap();
        assert_eq!(resumed.prior.len(), 1);
        assert!(std::fs::read(&partial).unwrap().ends_with(b"\n"));
        resumed.ledger.finish().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interior_damage_rejects() {
        let root = std::env::temp_dir().join(format!(
            "hl-resume-damage-{}-{}",
            std::process::id(),
            SCRATCH.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&root).unwrap();
        let report = root.join("result.tsv");
        let first = open(&report, "stamp", vec![case()], false).unwrap();
        drop(first);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(report.with_extension("partial.tsv"))
            .unwrap();
        file.write_all(b"malformed\n").unwrap();
        file.sync_data().unwrap();
        drop(file);
        assert!(open(&report, "stamp", vec![case()], true).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_owner_rejects() {
        let root = std::env::temp_dir().join(format!(
            "hl-resume-owner-{}-{}",
            std::process::id(),
            SCRATCH.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&root).unwrap();
        let report = root.join("result.tsv");
        let first = open(&report, "stamp", vec![case()], false).unwrap();
        assert!(open(&report, "stamp", vec![case()], true).is_err());
        drop(first);
        assert!(open(&report, "stamp", vec![case()], true).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
}
