//! Runtime compatibility sweep.
//!
//! A corpus mark is only comparable to another mark taken the same way, so record one with:
//! `cargo run --release -p testing --bin testing -- runtime --jobs 4 --results
//! target/testing/runtime/mark.tsv --engine-profile release`, having built `hl-engine` release
//! first. Low `--jobs` keeps the `host_load` column near the core count; a mark taken at a load
//! above the core count is contended and its timeouts are not engine defects. Counts also differ
//! by host: `!host-excluded` cases are `NOT_RUN` on the hosts they name and active everywhere
//! else, so quote `pass`/`fail`/`NOT_RUN` beside the host and the `host_load` distribution.
//!
//! The last recorded mark is committed at `tests/runtime/baseline.tsv`. Add `--baseline` and the
//! sweep diffs against it and fails only on a case that moved the wrong way, which answers "did I
//! regress anything" without re-deriving a full sweep to compare against.

mod baseline;
#[cfg(test)]
mod case_tests;
pub(crate) mod definition;
mod diagnostic;
mod execution;
mod fingerprint;
pub(crate) mod image;
mod ledger;
pub(crate) mod load;
pub(crate) mod profile;
pub(crate) mod scheduler;

use crate::suite::{Error, Target};
use clap::Args;
use definition::{App, EngineHost};
use diagnostic::BoundedDiagnostic as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) use execution::WorkerOptions;

pub(crate) fn preflight_image(name: &str, target: Target) -> Result<bool, Error> {
    image::ImageCache::for_platform(&target.platform())?.preflight(name)
}

pub async fn run(options: Options) -> Result<(), Error> {
    options.engine_profile.require()?;
    let runner = profile::identity()?;
    println!("runtime: engine profile={} runner={}", profile::PROFILE, &runner[..16]);
    let apps = apps(&options)?;
    validate_case_ids(&apps)?;
    let planned = require_planned(plan(apps, &options), options.selection.case.as_deref())?;
    let mut work = planned.work;
    let skipped = planned.skipped;
    // The runner identity joins the stamp so a rebuilt engine cannot resume the previous one's rows.
    let stamp = format!(
        "{}\t{}\t{runner}",
        fingerprint::calculate(&work).await?,
        profile::PROFILE
    );
    let keys: std::collections::BTreeSet<WorkKey> = work
        .iter()
        .map(|item| item.key.clone())
        .chain(skipped.iter().map(|row| row.attempt.key.clone()))
        .collect();
    let report = workspace()?.join(&options.results);
    let resume = options.selection.resume;
    let opened = tokio::task::spawn_blocking(move || {
        ledger::Ledger::open(&report, &stamp, &keys, resume).map_err(|error| error.to_string())
    })
    .await??;
    let ledger = Arc::new(opened.ledger);
    let mut prior = opened.prior;
    // A NOT_RUN row records an unattempted case, so resume must retry rather than accept it.
    prior.retain(|_, row| row.attempt.status != ledger::NOT_RUN);
    record_all(&ledger, skipped).await?;
    work.retain(|item| !prior.contains_key(&item.key));
    let mut running = crate::pool::Pool::new(work, options.selection.jobs);
    let drained = drain(&mut running, &ledger).await;
    // The report is published even when the sweep aborts, so no case is silently absent.
    backfill(&ledger, drained.as_ref().err()).await;
    let published = tokio::task::spawn_blocking({
        let ledger = Arc::clone(&ledger);
        move || ledger.finish().map_err(|error| error.to_string())
    })
    .await?;
    let mut completed = drained?;
    published?;
    completed.sort_by(|left, right| left.key.cmp(&right.key));
    let contended = contended(&prior, &completed);
    let (passed, failed) = summarize(&prior, completed);
    println!(
        "runtime: {passed} passed; {} failed; profile={}",
        failed.len(),
        profile::PROFILE
    );
    if contended > 0 {
        println!("runtime: SUSPECT {contended} case(s) ran under a saturated host; their elapsed_ms is not comparable");
    }
    match &options.baseline {
        Some(mark) => compare(&workspace()?.join(mark), &workspace()?.join(&options.results)),
        None if failed.is_empty() => Ok(()),
        None => Err(failed.join("\n").into()),
    }
}

/// Judges the published ledger against a recorded mark: only a row that moved the wrong way fails
/// the sweep, so a known failing set stays green and a new failure cannot hide inside the count.
fn compare(mark: &Path, results: &Path) -> Result<(), Error> {
    let mark = baseline::load(mark)?;
    println!("runtime: baseline {}", mark.describe());
    let changes = mark.diff(&baseline::observed(results)?);
    if changes.is_empty() {
        println!("runtime: no case moved against the baseline");
        return Ok(());
    }
    // A regressing row is printed by the error below, so printing it here too double-counts it: a
    // lane read one regression as two on first pass. The summary carries only what the error omits.
    for change in changes.iter().filter(|change| !change.regression) {
        println!("runtime: {}", change.line);
    }
    let regressions: Vec<&str> = changes
        .iter()
        .filter(|change| change.regression)
        .map(|change| change.line.as_str())
        .collect();
    if regressions.is_empty() {
        Ok(())
    } else {
        Err(regressions.join("\n").into())
    }
}

/// Best effort: a backfill failure must never replace the abort that caused it.
async fn backfill(ledger: &Arc<ledger::Ledger>, aborted: Option<&Error>) {
    match unattempted(ledger, aborted) {
        Ok(rows) => {
            if let Err(error) = record_all(ledger, rows).await {
                eprintln!("runtime: recording unattempted cases failed: {error}");
            }
        }
        Err(error) => eprintln!("runtime: enumerating unattempted cases failed: {error}"),
    }
}

/// Records every planned key the sweep never reached, so absence is never mistaken for success.
fn unattempted(ledger: &ledger::Ledger, aborted: Option<&Error>) -> Result<Vec<ledger::Row>, Error> {
    let recorded = ledger.recorded()?;
    let reason = aborted.map_or_else(
        || String::from("case was not scheduled"),
        |error| format!("sweep aborted before this case ran: {error}"),
    );
    Ok(ledger
        .planned()
        .difference(&recorded)
        .map(|key| ledger::Row {
            attempt: crate::journal::Attempt {
                key: key.clone(),
                status: ledger::NOT_RUN,
                elapsed_ms: 0,
            },
            host_load: load::unmeasured(),
            diagnostic: reason.clone(),
        })
        .collect())
}

async fn record_all(ledger: &Arc<ledger::Ledger>, rows: Vec<ledger::Row>) -> Result<(), Error> {
    let recording = Arc::clone(ledger);
    tokio::task::spawn_blocking(move || {
        for row in rows {
            recording.record(row).map_err(|error| error.to_string())?;
        }
        Ok::<(), String>(())
    })
    .await??;
    Ok(())
}

pub async fn worker(options: WorkerOptions) -> Result<(), Error> {
    execution::worker(options).await
}

fn worker_work(app: String, case: String, target: Target) -> Result<Work, Error> {
    let options = Options {
        app: Some(app),
        selection: crate::suite::Selection::exact(case.clone(), target),
        results: PathBuf::from("target/testing/runtime/worker.tsv"),
        baseline: None,
        engine_profile: profile::Requested::Release,
    };
    let apps = apps(&options)?;
    validate_case_ids(&apps)?;
    let mut planned = require_planned(plan(apps, &options), Some(&case))?.work;
    if planned.len() != 1 {
        return Err("runtime worker selection did not resolve exactly one row".into());
    }
    Ok(planned.remove(0))
}

async fn execute(work: Work) -> Completion {
    let started = std::time::Instant::now();
    let result = execution::run_case(Arc::clone(&work.app), work.case_index, work.target)
        .await
        .map_err(|error| error.to_string());
    Completion {
        key: work.key,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        host_load: load::sample(),
        result,
    }
}

async fn drain(
    running: &mut crate::pool::Pool<Work, Completion>,
    ledger: &Arc<ledger::Ledger>,
) -> Result<Vec<Completion>, Error> {
    let mut completed = Vec::new();
    while let Some(result) = running.next(execute).await? {
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

struct Completion {
    key: WorkKey,
    elapsed_ms: u64,
    host_load: String,
    result: Result<execution::Report, String>,
}

impl Completion {
    fn row(&self) -> ledger::Row {
        let (status, diagnostic) = outcome(&self.result);
        ledger::Row {
            attempt: crate::journal::Attempt {
                key: self.key.clone(),
                status,
                elapsed_ms: self.elapsed_ms,
            },
            host_load: self.host_load.clone(),
            diagnostic,
        }
    }
}

/// The row's status and its diagnostic, which carries the engine counters even on a pass so the
/// sweep stays auditable after the fact.
fn outcome(result: &Result<execution::Report, String>) -> (&'static str, String) {
    let (status, failure, counters) = match result {
        Ok(report) if report.results.iter().all(execution::CaseResult::passed) => {
            (ledger::PASS, String::new(), report.counters.as_str())
        }
        Ok(report) => (
            ledger::FAIL,
            report
                .results
                .iter()
                .filter_map(execution::CaseResult::diagnostic)
                .collect::<Vec<_>>()
                .join("; "),
            report.counters.as_str(),
        ),
        Err(error) => (ledger::FAIL, error.clone(), ""),
    };
    let diagnostic = match (failure.is_empty(), counters.is_empty()) {
        (_, true) => failure,
        (true, false) => counters.to_owned(),
        (false, false) => format!("{failure} || {counters}"),
    };
    (status, diagnostic.bounded_to(diagnostic::DIAGNOSTIC_LIMIT))
}

/// Counts rows whose recorded `host_load` makes their timing untrustworthy.
fn contended(prior: &std::collections::BTreeMap<WorkKey, ledger::Row>, completed: &[Completion]) -> usize {
    prior
        .values()
        .map(|row| row.host_load.as_str())
        .chain(completed.iter().map(|result| result.host_load.as_str()))
        .filter(|sample| load::saturated(sample))
        .count()
}

fn summarize(
    prior: &std::collections::BTreeMap<WorkKey, ledger::Row>,
    completed: Vec<Completion>,
) -> (usize, Vec<String>) {
    let mut passed = 0;
    let mut failed = Vec::new();
    for row in prior.values() {
        println!(
            "RESUME {} {} {} elapsed_ms={} host_load={}",
            row.attempt.status,
            row.attempt.key.id,
            row.attempt.key.target.name(),
            row.attempt.elapsed_ms,
            row.host_load
        );
        if row.attempt.status == "pass" {
            passed += 1;
        } else {
            failed.push(format!(
                "{} {}: {}",
                row.attempt.key.id,
                row.attempt.key.target.name(),
                row.diagnostic
            ));
        }
    }
    for result in completed {
        summarize_completed(result, &mut passed, &mut failed);
    }
    (passed, failed)
}

fn summarize_completed(completed: Completion, passed: &mut usize, failed: &mut Vec<String>) {
    match completed.result {
        Ok(report) => summarize_results(
            &completed.key,
            completed.elapsed_ms,
            &completed.host_load,
            report.results,
            passed,
            failed,
        ),
        Err(error) => {
            let failure = format!(
                "{} {} host_load={}: {error}",
                completed.key.id,
                completed.key.target.name(),
                completed.host_load
            );
            println!("FAIL {failure}");
            failed.push(failure);
        }
    }
}

fn summarize_results(
    key: &WorkKey,
    elapsed_ms: u64,
    host_load: &str,
    results: Vec<execution::CaseResult>,
    passed: &mut usize,
    failed: &mut Vec<String>,
) {
    for result in results {
        match result {
            execution::CaseResult::Passed(id, attempt) => {
                println!(
                    "PASS {} {} elapsed_ms={elapsed_ms} host_load={host_load}",
                    display_attempt(&id, attempt),
                    key.target.name()
                );
                *passed += 1;
            }
            execution::CaseResult::Failed(id, attempt, error) => {
                let display = display_attempt(&id, attempt);
                println!("FAIL {display} {} host_load={host_load}: {error}", key.target.name());
                failed.push(format!(
                    "{display} {} host_load={host_load}: {error}",
                    key.target.name()
                ));
            }
        }
    }
}

struct Schedule {
    work: Vec<Work>,
    skipped: Vec<ledger::Row>,
    matched_case: bool,
}

fn plan(apps: Vec<App>, options: &Options) -> Schedule {
    plan_for_host(apps, options, EngineHost::current())
}

fn plan_for_host(apps: Vec<App>, options: &Options, host: EngineHost) -> Schedule {
    let mut work = Vec::new();
    let mut skipped = Vec::new();
    let mut matched_case = options.selection.case.is_none();
    for app in apps {
        let app = Arc::new(app);
        if let Some(selected) = options.selection.case.as_deref()
            && app.cases.iter().any(|case| case.id == selected)
        {
            matched_case = true;
        }
        for target in options.selection.targets() {
            if !app.supports(target) {
                continue;
            }
            for (case_index, case) in app.cases.iter().enumerate() {
                if options
                    .selection
                    .case
                    .as_deref()
                    .is_some_and(|selected| case.id != selected)
                {
                    continue;
                }
                if !case.targets.contains(&target) {
                    continue;
                }
                if let Some((kind, reason, evidence)) = case.inactive(host) {
                    println!("{kind} {} {}: {reason} [{evidence}]", case.id, target.name());
                    skipped.push(ledger::Row {
                        attempt: crate::journal::Attempt {
                            key: WorkKey {
                                id: case.id.clone(),
                                target,
                            },
                            status: ledger::NOT_RUN,
                            elapsed_ms: 0,
                        },
                        host_load: load::unmeasured(),
                        diagnostic: format!("{kind}: {reason} [{evidence}]"),
                    });
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
    Schedule {
        work,
        skipped,
        matched_case,
    }
}

/// A selection with only inactive cases is a valid, fully recorded `NOT_RUN` sweep, not a failure.
fn require_planned(planned: Schedule, selected: Option<&str>) -> Result<Schedule, Error> {
    if !planned.matched_case {
        let selected = selected.ok_or("case selection match state is inconsistent")?;
        return Err(format!("no runtime case exactly matched --case {selected}").into());
    }
    if planned.work.is_empty() && planned.skipped.is_empty() {
        return Err(selected
            .map_or_else(
                || "no runtime cases support the selected target(s)".to_owned(),
                |id| format!("runtime case {id} matched but has no work for the selected target(s)"),
            )
            .into());
    }
    Ok(planned)
}

fn display_attempt(id: &str, attempt: Option<u16>) -> String {
    attempt.map_or_else(|| id.to_owned(), |ordinal| format!("{id}#attempt-{ordinal}"))
}

pub fn oracle(options: OracleOptions) -> Result<(), Error> {
    let apps = apps(&options.runtime)?;
    validate_case_ids(&apps)?;
    if let Some(selected) = options.runtime.selection.case.as_deref()
        && !apps.iter().any(|app| app.cases.iter().any(|case| case.id == selected))
    {
        return Err(format!("no runtime case exactly matched --case {selected}").into());
    }
    let mut eligible = false;
    for app in apps {
        for target in options.runtime.selection.targets() {
            if !app.supports(target) {
                continue;
            }
            let mut cases = app.cases_for(target);
            if !cases.any(|case| {
                options
                    .runtime
                    .selection
                    .case
                    .as_deref()
                    .is_none_or(|selected| case.id == selected)
            }) {
                continue;
            }
            eligible = true;
            app.oracle(target, options.update, options.runtime.selection.case.as_deref())?;
        }
    }
    if eligible {
        Ok(())
    } else {
        Err("no oracle cases support the selected target(s)".into())
    }
}

fn validate_case_ids(apps: &[App]) -> Result<(), Error> {
    let mut ids = std::collections::BTreeSet::new();
    for case in apps.iter().flat_map(|app| &app.cases) {
        if !ids.insert(&case.id) {
            return Err(format!("runtime case ID is duplicated: {}", case.id).into());
        }
    }
    Ok(())
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
    #[command(flatten)]
    selection: crate::suite::Selection,
    /// Relative durable result path beneath the repository workspace.
    #[arg(long, default_value = "target/testing/runtime/results.tsv", value_parser = crate::suite::parse::results)]
    results: PathBuf,
    /// Diff the sweep against a recorded corpus mark instead of against "everything passes".
    #[arg(
        long,
        num_args = 0..=1,
        default_missing_value = "tests/runtime/baseline.tsv",
        value_parser = crate::suite::parse::results,
    )]
    baseline: Option<PathBuf>,
    /// Engine build profile this sweep is measuring; must match how the runner was built.
    #[arg(long, value_enum, env = "HL_COMPAT_ENGINE_PROFILE", default_value_t = profile::Requested::Release)]
    engine_profile: profile::Requested,
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
    runtime: Options,
}

#[cfg(test)]
mod tests {
    use super::{WorkKey, display_attempt, ledger, unattempted};
    use crate::journal::Attempt;
    use crate::suite::Target;

    #[test]
    fn an_abort_records_every_unreached_case_rather_than_dropping_it() {
        let key = |id: &str| WorkKey {
            id: id.to_owned(),
            target: Target::Arm64,
        };
        let keys = std::collections::BTreeSet::from([key("runtime/a"), key("runtime/b")]);
        let directory = tempfile::tempdir().unwrap();
        let opened = ledger::Ledger::open(&directory.path().join("results.tsv"), "stamp", &keys, false).unwrap();
        opened
            .ledger
            .record(ledger::Row {
                attempt: Attempt {
                    key: key("runtime/a"),
                    status: ledger::PASS,
                    elapsed_ms: 1,
                },
                host_load: "0.10/8".to_owned(),
                diagnostic: String::new(),
            })
            .unwrap();
        let rows = unattempted(&opened.ledger, Some(&"row limit".into())).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].attempt.key.id, "runtime/b");
        assert_eq!(rows[0].attempt.status, ledger::NOT_RUN);
        assert!(rows[0].diagnostic.contains("row limit"), "{}", rows[0].diagnostic);
    }

    #[test]
    fn attempt_display_does_not_mutate_the_case_identity() {
        let id = String::from("runtime/soak");
        assert_eq!(display_attempt(&id, None), "runtime/soak");
        assert_eq!(display_attempt(&id, Some(7)), "runtime/soak#attempt-7");
        assert_eq!(id, "runtime/soak");
    }
}
