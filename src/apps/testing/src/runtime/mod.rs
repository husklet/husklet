pub(crate) mod definition;
mod execution;
pub(crate) mod image;
mod ledger;
pub(crate) mod scheduler;

use crate::suite::{Error, Target};
use clap::Args;
use definition::App;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::{sync::Semaphore, task::JoinSet};

pub async fn run(options: Options) -> Result<(), Error> {
    let mut work = plan(apps(&options)?, &options);
    if work.is_empty() {
        return Err("no active runtime cases support the selected target(s)".into());
    }
    let stamp = fingerprint(&work).await?;
    let keys = work.iter().map(|item| item.key.clone()).collect();
    let report = workspace()?.join(&options.results);
    let resume = options.resume;
    let opened = tokio::task::spawn_blocking(move || {
        ledger::Ledger::open(&report, &stamp, &keys, resume).map_err(|error| error.to_string())
    })
    .await??;
    let ledger = Arc::new(opened.ledger);
    let prior = opened.prior;
    work.retain(|item| !prior.contains_key(&item.key));
    let semaphore = Arc::new(Semaphore::new(options.jobs));
    let mut running = JoinSet::new();
    for work in work {
        spawn(&mut running, work, Arc::clone(&semaphore));
    }
    let mut completed = drain(&mut running, &ledger).await?;
    completed.sort_by(|left, right| left.key.cmp(&right.key));
    let (passed, failed) = summarize(&prior, completed);
    tokio::task::spawn_blocking(move || ledger.finish().map_err(|error| error.to_string())).await??;
    println!("runtime: {passed} passed; {} failed", failed.len());
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("\n").into())
    }
}

fn spawn(running: &mut JoinSet<Result<Completed, String>>, work: Work, semaphore: Arc<Semaphore>) {
    running.spawn(async move {
        let _permit = semaphore
            .acquire_owned()
            .await
            .map_err(|_| "runtime worker pool closed".to_owned())?;
        let started = std::time::Instant::now();
        let result = execution::run_case(Arc::clone(&work.app), work.case_index, work.target)
            .await
            .map_err(|error| error.to_string());
        Ok(Completed {
            key: work.key,
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            result,
        })
    });
}

async fn drain(running: &mut JoinSet<Result<Completed, String>>, ledger: &Arc<ledger::Ledger>) -> Result<Vec<Completed>, Error> {
    let mut completed = Vec::new();
    while let Some(result) = running.join_next().await {
        let result = result?.map_err(|error| -> Error { error.into() })?;
        let row = result.row();
        let recording = Arc::clone(ledger);
        tokio::task::spawn_blocking(move || recording.record(row).map_err(|error| error.to_string())).await??;
        completed.push(result);
    }
    Ok(completed)
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct WorkKey {
    id: String,
    target: Target,
}

struct Work {
    key: WorkKey,
    app: Arc<App>,
    case_index: usize,
    target: Target,
}

struct Completed {
    key: WorkKey,
    elapsed_ms: u64,
    result: Result<Vec<execution::CaseResult>, String>,
}

impl Completed {
    fn row(&self) -> ledger::Row {
        let (status, diagnostic) = outcome(&self.result);
        ledger::Row {
            key: self.key.clone(),
            status,
            elapsed_ms: self.elapsed_ms,
            diagnostic,
        }
    }
}

fn outcome(result: &Result<Vec<execution::CaseResult>, String>) -> (&'static str, String) {
    match result {
        Ok(results) if results.iter().all(execution::CaseResult::passed) => ("pass", String::new()),
        Ok(results) => ("fail", results.iter().filter_map(execution::CaseResult::diagnostic).collect::<Vec<_>>().join("; ")),
        Err(error) => ("fail", error.clone()),
    }
}

fn summarize(prior: &std::collections::BTreeMap<WorkKey, ledger::Row>, completed: Vec<Completed>) -> (usize, Vec<String>) {
    let mut passed = 0;
    let mut failed = Vec::new();
    for row in prior.values() {
        println!("RESUME {} {} {} elapsed_ms={}", row.status, row.key.id, row.key.target.name(), row.elapsed_ms);
        if row.status == "pass" {
            passed += 1;
        } else {
            failed.push(format!("{} {}: {}", row.key.id, row.key.target.name(), row.diagnostic));
        }
    }
    for result in completed {
        summarize_completed(result, &mut passed, &mut failed);
    }
    (passed, failed)
}

fn summarize_completed(completed: Completed, passed: &mut usize, failed: &mut Vec<String>) {
    match completed.result {
        Ok(results) => summarize_results(&completed.key, completed.elapsed_ms, results, passed, failed),
        Err(error) => {
            let failure = format!("{} {}: {error}", completed.key.id, completed.key.target.name());
            println!("FAIL {failure}");
            failed.push(failure);
        }
    }
}

fn summarize_results(
    key: &WorkKey,
    elapsed_ms: u64,
    results: Vec<execution::CaseResult>,
    passed: &mut usize,
    failed: &mut Vec<String>,
) {
    for result in results {
        match result {
            execution::CaseResult::Passed(id, attempt) => {
                println!("PASS {} {} elapsed_ms={elapsed_ms}", display_attempt(&id, attempt), key.target.name());
                *passed += 1;
            }
            execution::CaseResult::Failed(id, attempt, error) => {
                let display = display_attempt(&id, attempt);
                println!("FAIL {display} {}: {error}", key.target.name());
                failed.push(format!("{display} {}: {error}", key.target.name()));
            }
        }
    }
}

fn plan(apps: Vec<App>, options: &Options) -> Vec<Work> {
    let mut work = Vec::new();
    for app in apps {
        let app = Arc::new(app);
        for target in options.targets() {
            if !app.supports(target) {
                continue;
            }
            for (case_index, case) in app.cases.iter().enumerate() {
                if !case.targets.contains(&target) {
                    continue;
                }
                if let Some((kind, reason, evidence)) = case.inactive() {
                    println!("{kind} {} {}: {reason} [{evidence}]", case.id, target.name());
                    continue;
                }
                work.push(Work {
                    key: WorkKey {
                        id: case.id.clone(),
                        target,
                    },
                    app: Arc::clone(&app),
                    case_index,
                    target,
                });
            }
        }
    }
    work.sort_by(|left, right| left.key.cmp(&right.key));
    work
}

fn display_attempt(id: &str, attempt: Option<u16>) -> String {
    attempt.map_or_else(|| id.to_owned(), |ordinal| format!("{id}#attempt-{ordinal}"))
}

async fn fingerprint(work: &[Work]) -> Result<String, Error> {
    let inputs = work
        .iter()
        .map(|item| {
            let case = &item.app.cases[item.case_index];
            (
                item.key.clone(),
                item.app.directory.join(&case.source),
                case.golden.clone(),
                format!(
                    "{}\0{}\0{:?}\0{:?}\0{:?}\0{}\0{}\0{:?}\0{:?}\0{:?}\0{:?}",
                    item.app.image,
                    item.app.compiler_name(item.target),
                    item.app.execution,
                    case.arguments,
                    case.environment,
                    case.timeout,
                    case.exit,
                    case.flags,
                    case.destination,
                    case.compat,
                    case.soak
                ),
            )
        })
        .collect::<Vec<_>>();
    tokio::task::spawn_blocking(move || {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        for (key, source, golden, metadata) in inputs {
            digest.update(key.id.as_bytes());
            digest.update([0]);
            digest.update(key.target.name().as_bytes());
            digest.update([0]);
            digest.update(metadata.as_bytes());
            digest.update(std::fs::read(source).map_err(|error| error.to_string())?);
            digest.update(std::fs::read(golden).map_err(|error| error.to_string())?);
        }
        Ok::<_, String>(digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
    })
    .await?
    .map_err(Into::into)
}

pub fn oracle(options: OracleOptions) -> Result<(), Error> {
    let _check_requested = options.check;
    let mut eligible = false;
    for app in apps(&options.selection)? {
        for target in options.selection.targets() {
            if !app.supports(target) {
                continue;
            }
            if app.cases_for(target).next().is_none() {
                continue;
            }
            eligible = true;
            app.oracle(target, options.update)?;
        }
    }
    if eligible {
        Ok(())
    } else {
        Err("no oracle cases support the selected target(s)".into())
    }
}

fn apps(options: &Options) -> Result<Vec<App>, Error> {
    let root = workspace()?.join("tests/runtime");
    let mut directories = std::fs::read_dir(&root)?
        .map(|entry| entry.map(|value| value.path()))
        .collect::<Result<Vec<_>, _>>()?;
    directories.sort();
    let mut result = Vec::new();
    for directory in directories.into_iter().filter(|value| value.is_dir()) {
        let name = directory
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if options.app.as_deref().is_some_and(|selected| selected != name) {
            continue;
        }
        let definition = directory.join("test.yaml");
        if definition.is_file() {
            result.push(App::load(&directory, &definition)?);
        }
    }
    if result.is_empty() {
        return Err(format!("no runtime apps matched under {}", root.display()).into());
    }
    Ok(result)
}

pub(super) fn workspace() -> Result<PathBuf, Error> {
    let mut path = Path::new(env!("CARGO_MANIFEST_DIR"));
    while !path.join("tests/runtime").is_dir() {
        path = path.parent().ok_or("workspace root not found")?;
    }
    Ok(path.to_path_buf())
}

#[derive(Args)]
pub(crate) struct Options {
    /// Run only the named runtime application.
    app: Option<String>,
    /// Run only one guest ISA.
    #[arg(long = "isa", value_enum)]
    target: Option<Target>,
    /// Maximum number of concurrently executing cases.
    #[arg(long, env = "HL_COMPAT_JOBS", default_value_t = logical_jobs(), value_parser = parse_jobs)]
    jobs: usize,
    /// Resume exact completed case/target keys from the synchronized partial result.
    #[arg(long, env = "HL_COMPAT_RESUME", default_value_t = false)]
    resume: bool,
    /// Relative durable result path beneath the repository workspace.
    #[arg(long, default_value = "target/testing/runtime/results.tsv", value_parser = parse_results)]
    results: PathBuf,
}

fn logical_jobs() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

fn parse_jobs(value: &str) -> Result<usize, String> {
    let jobs = value
        .parse::<usize>()
        .map_err(|_| "jobs must be an integer".to_owned())?;
    (1..=256)
        .contains(&jobs)
        .then_some(jobs)
        .ok_or_else(|| "jobs must be between 1 and 256".to_owned())
}

fn parse_results(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        Err("results must be a safe relative path".to_owned())
    } else {
        Ok(path)
    }
}

impl Options {
    fn targets(&self) -> Vec<Target> {
        self.target
            .map_or_else(|| vec![Target::Arm64, Target::Amd64], |value| vec![value])
    }
}

#[derive(Args)]
pub(crate) struct OracleOptions {
    /// Replace checked golden output with oracle output.
    #[arg(long, conflicts_with = "check")]
    update: bool,
    /// Check oracle output against the golden (the default).
    #[arg(long)]
    check: bool,
    #[command(flatten)]
    selection: Options,
}

#[cfg(test)]
mod tests {
    use super::display_attempt;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::{Duration, Instant};
    use tokio::{sync::Semaphore, task::JoinSet};

    #[test]
    fn attempt_display_does_not_mutate_the_case_identity() {
        let id = String::from("runtime/soak");
        assert_eq!(display_attempt(&id, None), "runtime/soak");
        assert_eq!(display_attempt(&id, Some(7)), "runtime/soak#attempt-7");
        assert_eq!(id, "runtime/soak");
    }

    #[tokio::test]
    async fn worker_bound_limits_concurrency_without_serializing() {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let semaphore = Arc::new(Semaphore::new(2));
        let mut tasks = JoinSet::new();
        let started = Instant::now();
        for ordinal in 0..6 {
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            let semaphore = Arc::clone(&semaphore);
            tasks.spawn(async move {
                let _permit = semaphore.acquire_owned().await.unwrap();
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(current, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                ordinal
            });
        }
        let mut order = Vec::new();
        while let Some(result) = tasks.join_next().await {
            order.push(result.unwrap());
        }
        order.sort_unstable();
        assert_eq!(maximum.load(Ordering::SeqCst), 2);
        assert!(started.elapsed() >= Duration::from_millis(55));
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(order, (0..6).collect::<Vec<_>>());
    }
}
