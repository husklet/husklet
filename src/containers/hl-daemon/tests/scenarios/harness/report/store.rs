use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
};

use super::schema::{
    Attempt, BatchReport, ScenarioKey, ScenarioOutcome, Status, WorkflowAttempt, WorkflowKey,
    WorkflowOutcome,
};

static TEMPORARY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub struct Store {
    root: PathBuf,
    results: PathBuf,
}
impl Store {
    pub fn from_env() -> io::Result<Option<Self>> {
        let Some(run_id) = std::env::var_os("HL_SCENARIO_RUN_ID") else {
            return Ok(None);
        };
        let base = std::env::var_os("HL_SCENARIO_REPORT_DIR").map_or_else(
            || {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .expect("hl-daemon belongs to a workspace")
                    .join(".cache/scenario-runs")
            },
            PathBuf::from,
        );
        Self::create(&base, &run_id.to_string_lossy()).map(Some)
    }
    pub fn create(base: &Path, run_id: &str) -> io::Result<Self> {
        let root = base.join(run_id);
        fs::create_dir_all(root.join("logs"))?;
        let store = Self {
            results: root.join("results.jsonl"),
            root,
        };
        Self::update_latest(base, run_id)?;
        Ok(store)
    }
    fn update_latest(base: &Path, run_id: &str) -> io::Result<()> {
        atomic_write(&base.join("latest"), run_id.as_bytes())
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn log_path(&self, id: &str) -> PathBuf {
        let safe = id
            .chars()
            .map(|value| {
                if value.is_ascii_alphanumeric() || matches!(value, '-' | '_') {
                    value
                } else {
                    '_'
                }
            })
            .collect::<String>();
        self.root.join("logs").join(format!("{safe}.log"))
    }
    pub fn append(&self, outcome: &ScenarioOutcome) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(outcome).map_err(io::Error::other)?;
        bytes.push(b'\n');
        append_bytes(&self.results, &bytes)
    }
    /// Appends corrections without replacing or truncating historical results.
    pub fn invalidate(
        &self,
        scenarios: &[String],
        archive: &str,
        reason: &str,
    ) -> io::Result<usize> {
        let current = self.resume()?;
        let mut appended = 0;
        for outcome in current.values().filter(|outcome| {
            outcome.key.engine_archive_hash == archive && scenarios.contains(&outcome.key.scenario)
        }) {
            let mut correction = outcome.clone();
            correction.status = Status::InfrastructureFail;
            correction.error = Some(format!("invalid_fixture: {reason}"));
            self.append_line(&correction)?;
            appended += 1;
        }
        Ok(appended)
    }
    fn append_line(&self, outcome: &ScenarioOutcome) -> io::Result<()> {
        self.append(outcome)
    }
    pub fn begin(&self, attempt: &Attempt) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(attempt).map_err(io::Error::other)?;
        bytes.push(b'\n');
        append_bytes(&self.root.join("attempts.jsonl"), &bytes)
    }
    pub fn begin_workflow(&self, attempt: &WorkflowAttempt) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(attempt).map_err(io::Error::other)?;
        bytes.push(b'\n');
        append_bytes(&self.root.join("workflow-attempts.jsonl"), &bytes)
    }
    pub fn append_workflow(&self, outcome: &WorkflowOutcome) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(outcome).map_err(io::Error::other)?;
        bytes.push(b'\n');
        append_bytes(&self.root.join("workflow-results.jsonl"), &bytes)
    }
    pub fn write_log(path: &Path, text: &str) -> io::Result<()> {
        atomic_write(path, text.as_bytes())
    }
    pub fn resume(&self) -> io::Result<BTreeMap<ScenarioKey, ScenarioOutcome>> {
        let bytes = fs::read(&self.results).unwrap_or_default();
        let mut values = BTreeMap::new();
        for line in bytes.split(|b| *b == b'\n').filter(|v| !v.is_empty()) {
            let value: ScenarioOutcome = serde_json::from_slice(line).map_err(io::Error::other)?;
            values.insert(value.key.clone(), value);
        }
        Ok(values)
    }
    pub fn resume_workflows(&self) -> io::Result<BTreeMap<WorkflowKey, WorkflowOutcome>> {
        let bytes = fs::read(self.root.join("workflow-results.jsonl")).unwrap_or_default();
        let mut values = BTreeMap::new();
        for line in bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let value: WorkflowOutcome = serde_json::from_slice(line).map_err(io::Error::other)?;
            values.insert(value.key.clone(), value);
        }
        Ok(values)
    }
    pub fn finish(&self, report: &BatchReport) -> io::Result<()> {
        if std::env::var_os("HL_SCENARIO_CHILD").is_some() {
            return Ok(());
        }
        atomic_write(
            &self.root.join("summary.json"),
            &serde_json::to_vec_pretty(report).map_err(io::Error::other)?,
        )?;
        atomic_write(&self.root.join("summary.md"), report.markdown().as_bytes())
    }
}
fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let unique = TEMPORARY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp-{}-{unique}", std::process::id()));
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, path)
}

fn append_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    // One write prevents independently-running category workers from
    // interleaving the JSON value and its newline.
    file.write_all(bytes)?;
    file.sync_data()
}
