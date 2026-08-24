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
mod case_test;
pub(crate) mod definition;
mod diagnostic;
mod execution;
mod fingerprint;
pub(crate) mod image;
mod inventory_report;
mod ledger;
pub(crate) mod load;
mod options;
mod outcome;
mod output;
pub(crate) mod profile;
pub(crate) mod scheduler;
mod stage;
mod work_root;

use crate::suite::{Error, Target};
use definition::{App, EngineHost};
use diagnostic::BoundedDiagnostic as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) use execution::{WorkerOptions, worker};
pub(crate) use inventory_report::run as inventory;
pub(crate) use options::Options;
pub(crate) use stage::Options as StageOptions;
pub(crate) use stage::artifact_smoke;
pub(crate) use stage::run as stage;

pub(crate) fn preflight_image(name: &str, target: Target) -> Result<bool, Error> {
    image::ImageCache::for_platform(&target.platform())?.preflight(name)
}

pub async fn run(options: Options) -> Result<(), Error> {
    if options.broken_soak.is_some() && options.selection.case.is_none() {
        return Err("--broken-soak requires one exact --case".into());
    }
    if options.broken_soak.is_some() && options.baseline.is_some() {
        return Err(
            "--broken-soak writes repetition receipts and cannot be compared to the active-corpus baseline".into(),
        );
    }
    options.engine_profile.require()?;
    work_root::WorkRoot::configure(options.work_root.clone())?.preflight()?;
    let runner = profile::identity()?;
    println!("runtime: engine profile={} runner={}", profile::PROFILE, &runner[..16]);
    let apps = apps(&options)?;
    validate_case_ids(&apps)?;
    let planned = require_planned(Schedule::plan(apps, &options), options.selection.case.as_deref())?;
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
    let jobs = if options.broken_soak.is_some() {
        1
    } else {
        options.selection.jobs
    };
    let mut running = crate::pool::Pool::new(work, jobs);
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
        Some(mark) => baseline::compare(&workspace()?.join(mark), &workspace()?.join(&options.results)),
        None if failed.is_empty() => Ok(()),
        None => Err(failed.join("\n").into()),
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

fn worker_work(app: String, case: String, target: Target, allow_broken: bool) -> Result<Work, Error> {
    let options = Options {
        app: Some(app),
        selection: crate::suite::Selection::exact(case.clone(), target),
        results: PathBuf::from("target/testing/runtime/worker.tsv"),
        baseline: None,
        engine_profile: profile::Requested::Release,
        work_root: None,
        broken_soak: allow_broken.then_some(1),
    };
    let apps = apps(&options)?;
    validate_case_ids(&apps)?;
    let mut planned = require_planned(Schedule::plan(apps, &options), Some(&case))?.work;
    if planned.len() != 1 {
        return Err("runtime worker selection did not resolve exactly one row".into());
    }
    Ok(planned.remove(0))
}

async fn drain(
    running: &mut crate::pool::Pool<Work, Completion>,
    ledger: &Arc<ledger::Ledger>,
) -> Result<Vec<Completion>, Error> {
    let mut completed = Vec::new();
    loop {
        let result = match running.next(Work::execute).await {
            Ok(Some(result)) => result,
            Ok(None) => break,
            Err(error) => {
                running.shutdown().await;
                return Err(error.into());
            }
        };
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
    broken_soak: bool,
}

impl Work {
    async fn execute(self) -> Completion {
        let started = std::time::Instant::now();
        let result = execution::run_case(Arc::clone(&self.app), self.case_index, self.target, self.broken_soak)
            .await
            .map_err(|error| error.to_string());
        Completion {
            key: self.key,
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            host_load: load::sample(),
            result,
        }
    }
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

impl Schedule {
    fn plan(apps: Vec<App>, options: &Options) -> Self {
        Self::for_host(apps, options, EngineHost::current())
    }

    fn for_host(apps: Vec<App>, options: &Options, host: EngineHost) -> Self {
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
            plan_app(&app, options, host, &mut work, &mut skipped);
        }
        work.sort_by(|left, right| left.key.cmp(&right.key));
        Self {
            work,
            skipped,
            matched_case,
        }
    }
}

fn plan_app(app: &Arc<App>, options: &Options, host: EngineHost, work: &mut Vec<Work>, skipped: &mut Vec<ledger::Row>) {
    for target in options.selection.targets() {
        if !app.supports(target) {
            continue;
        }
        for case_index in 0..app.cases.len() {
            plan_case(app, case_index, target, options, host, work, skipped);
        }
    }
}

fn plan_case(
    app: &Arc<App>,
    case_index: usize,
    target: Target,
    options: &Options,
    host: EngineHost,
    work: &mut Vec<Work>,
    skipped: &mut Vec<ledger::Row>,
) {
    let case = &app.cases[case_index];
    if options
        .selection
        .case
        .as_deref()
        .is_some_and(|selected| case.id != selected)
        || !case.targets.contains(&target)
    {
        return;
    }
    if let Some((kind, reason, evidence)) = case.inactive(host) {
        if kind == "BROKEN"
            && let Some(repetitions) = options.broken_soak
        {
            for repetition in 1..=repetitions {
                work.push(Work {
                    key: WorkKey {
                        id: format!("{}#soak-{repetition:04}", case.id),
                        target,
                    },
                    app: Arc::clone(app),
                    case_index,
                    target,
                    broken_soak: true,
                });
            }
            return;
        }
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
        return;
    }
    work.push(Work {
        key: WorkKey {
            id: case.id.clone(),
            target,
        },
        app: Arc::clone(app),
        case_index,
        target,
        broken_soak: false,
    });
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

mod oracle;
pub(crate) use oracle::{OracleOptions, oracle};
use oracle::{apps, validate_case_ids};

pub(crate) fn workspace() -> Result<PathBuf, Error> {
    let mut path = Path::new(env!("CARGO_MANIFEST_DIR"));
    while !path.join("tests/runtime").is_dir() {
        path = path.parent().ok_or("workspace root not found")?;
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod test;
