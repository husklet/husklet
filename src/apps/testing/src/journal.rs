use crate::suite::Error;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::Mutex,
};

const FILE_LIMIT: u64 = 64 * 1024 * 1024;

/// States a condition the journal depends on, so callers read the claim rather than the negation.
pub(crate) trait Require {
    fn require(self, message: &'static str) -> Result<(), Error>;
}

impl Require for bool {
    fn require(self, message: &'static str) -> Result<(), Error> {
        self.then_some(()).ok_or_else(|| message.into())
    }
}

/// How one harness names, writes and reads the rows of its result journal.
pub(crate) trait Schema {
    type Key: Ord + Clone;
    type Row: Clone;

    /// Names the run in the journal stamp and in every diagnostic.
    const KIND: &'static str;
    const HEADER: &'static str;
    const ROW_LIMIT: usize;
    const FIELDS: usize;

    fn key(row: &Self::Row) -> &Self::Key;
    fn format(row: &Self::Row) -> Result<String, Error>;

    /// Reads one row, or `None` when it records work the current run has superseded.
    fn parse(fields: &[&str], keys: &BTreeSet<Self::Key>) -> Result<Option<Self::Row>, Error>;
}

pub(crate) struct Resumption<S: Schema> {
    pub ledger: Ledger<S>,
    pub prior: BTreeMap<S::Key, S::Row>,
}

/// A crash-durable result journal that resumes an interrupted run and publishes a sorted report.
pub(crate) struct Ledger<S: Schema> {
    report: PathBuf,
    partial: PathBuf,
    file: Mutex<Option<BufWriter<File>>>,
    rows: Mutex<BTreeMap<S::Key, S::Row>>,
    keys: BTreeSet<S::Key>,
    _lock: hl_engine::native::FileLock,
    _schema: PhantomData<fn() -> S>,
}

impl<S: Schema> Ledger<S> {
    pub(crate) fn open(
        report: &Path,
        stamp: &str,
        keys: &BTreeSet<S::Key>,
        resume: bool,
    ) -> Result<Resumption<S>, Error> {
        let partial = report.with_extension("partial.tsv");
        if let Some(parent) = report.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock = hl_engine::native::FileLock::acquire(report.with_extension("lock"))?;
        let prior = if resume && partial.exists() {
            Self::load(&partial, stamp, keys)?
        } else {
            Self::initialize(&partial, stamp)?;
            BTreeMap::new()
        };
        let file = OpenOptions::new().append(true).open(&partial)?;
        Ok(Resumption {
            ledger: Self {
                report: report.to_path_buf(),
                partial,
                file: Mutex::new(Some(BufWriter::new(file))),
                rows: Mutex::new(prior.clone()),
                keys: keys.clone(),
                _lock: lock,
                _schema: PhantomData,
            },
            prior,
        })
    }

    pub(crate) fn record(&self, row: S::Row) -> Result<(), Error> {
        let kind = S::KIND;
        let text = S::format(&row)?;
        if fs::metadata(&self.partial)?
            .len()
            .checked_add(text.len() as u64)
            .ok_or_else(|| format!("{kind} result file size overflow"))?
            > FILE_LIMIT
        {
            return Err(format!("{kind} result journal exceeded {FILE_LIMIT} bytes").into());
        }
        let mut guard = self.file.lock().map_err(|_| Self::poisoned("journal"))?;
        let file = guard
            .as_mut()
            .ok_or_else(|| format!("{kind} result journal is finalized"))?;
        file.write_all(text.as_bytes())?;
        file.flush()?;
        file.get_ref().sync_data()?;
        self.rows
            .lock()
            .map_err(|_| Self::poisoned("rows"))?
            .insert(S::key(&row).clone(), row);
        Ok(())
    }

    pub(crate) const fn planned(&self) -> &BTreeSet<S::Key> {
        &self.keys
    }

    pub(crate) fn recorded(&self) -> Result<BTreeSet<S::Key>, Error> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| Self::poisoned("rows"))?
            .keys()
            .cloned()
            .collect())
    }

    pub(crate) fn finish(&self) -> Result<(), Error> {
        let mut journal = self.file.lock().map_err(|_| Self::poisoned("journal"))?;
        if let Some(mut file) = journal.take() {
            file.flush()?;
            file.get_ref().sync_data()?;
        }
        let temporary = self.report.with_extension("tmp");
        let rows = self.rows.lock().map_err(|_| Self::poisoned("rows"))?;
        let mut file = BufWriter::new(File::create(&temporary)?);
        file.write_all(S::HEADER.as_bytes())?;
        for row in rows.values() {
            file.write_all(S::format(row)?.as_bytes())?;
        }
        file.flush()?;
        file.get_ref().sync_data()?;
        drop(file);
        fs::rename(&temporary, &self.report)?;
        fs::remove_file(&self.partial)?;
        Ok(())
    }

    fn poisoned(what: &str) -> String {
        format!("{} result {what} lock poisoned", S::KIND)
    }

    fn initialize(path: &Path, stamp: &str) -> Result<(), Error> {
        let mut file = BufWriter::new(File::create(path)?);
        writeln!(file, "# {}-run\t{stamp}", S::KIND)?;
        file.write_all(S::HEADER.as_bytes())?;
        file.flush()?;
        file.get_ref().sync_data()?;
        Ok(())
    }

    fn load(path: &Path, stamp: &str, keys: &BTreeSet<S::Key>) -> Result<BTreeMap<S::Key, S::Row>, Error> {
        let kind = S::KIND;
        let bytes = Self::truncate_torn_tail(path)?;
        let text = std::str::from_utf8(&bytes)?;
        let mut lines = text.lines();
        if lines.next() != Some(format!("# {kind}-run\t{stamp}").as_str()) {
            return Err(format!("{kind} resume input changed").into());
        }
        if lines.next() != Some(S::HEADER.trim_end()) {
            return Err(format!("{kind} resume schema changed").into());
        }
        let mut rows = BTreeMap::new();
        for line in lines.filter(|line| !line.is_empty()) {
            if line.len() > S::ROW_LIMIT {
                return Err(format!("{kind} resume row exceeds its byte bound").into());
            }
            let fields = line.split('\t').collect::<Vec<_>>();
            if fields.len() != S::FIELDS {
                return Err(format!("invalid {kind} resume row").into());
            }
            let Some(row) = S::parse(&fields, keys)? else {
                continue;
            };
            let key = S::key(&row).clone();
            if rows.insert(key, row).is_some() {
                return Err(format!("duplicate {kind} resume row").into());
            }
        }
        Ok(rows)
    }

    /// Drops an incomplete final line left by a crash so the journal parses.
    fn truncate_torn_tail(path: &Path) -> Result<Vec<u8>, Error> {
        let mut bytes = fs::read(path)?;
        if bytes.len() as u64 > FILE_LIMIT {
            return Err(format!("{} result journal exceeds its byte bound", S::KIND).into());
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
        Ok(bytes)
    }
}
