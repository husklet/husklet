mod definition;
mod execution;
mod ledger;
mod alternating;

use crate::{
    runtime,
    suite::{Error, Target},
};
use clap::Args;
use definition::Benchmark;
use std::{
    collections::BTreeSet,
    future::Future,
    path::{Component, PathBuf},
    sync::Arc,
};
use tokio::task::JoinSet;

const EVIDENCE_LIMIT: usize = 64 * 1024;
const DIAGNOSTIC_LIMIT: usize = 16 * 1024;
const LEDGER_IDENTITY: &str = "husklet-benchmark-ledger-v2";

pub async fn run(options: Options) -> Result<(), Error> {
    let planned = plan(options.benchmarks()?, &options);
    let mut preparations = WorkPool::new(planned, options.jobs);
    let mut work = Vec::new();
    while let Some(prepared) = preparations.next(prepare_work).await? {
        work.push(prepared?);
    }
    work.sort_by(|left, right| left.key.cmp(&right.key));
    let keys = work.iter().map(|item| item.key.clone()).collect::<BTreeSet<_>>();
    let report = runtime::workspace()?.join(&options.results);
    let resume = options.resume;
    let opened = tokio::task::spawn_blocking(move || {
        ledger::Ledger::open(&report, LEDGER_IDENTITY, &keys, resume).map_err(|error| error.to_string())
    })
    .await??;
    let ledger = Arc::new(opened.ledger);
    work.retain(|item| !opened.prior.contains_key(&item.key));
    let mut pool = WorkPool::new(work, options.jobs);
    let mut rows = opened.prior;
    while let Some(completed) = pool.next(execute_work).await? {
        let row = completed.row();
        let recording = Arc::clone(&ledger);
        let saved = row.clone();
        tokio::task::spawn_blocking(move || recording.record(saved).map_err(|error| error.to_string())).await??;
        rows.insert(row.key.clone(), row);
    }
    tokio::task::spawn_blocking(move || ledger.finish().map_err(|error| error.to_string())).await??;
    Report::finish(&rows)
}

async fn execute_work(work: Work) -> Completed {
    let started = std::time::Instant::now();
    let result = execution::run(work.benchmark, work.case_index, work.target, work.prepared).await;
    Completed {
        key: work.key,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        result,
    }
}

struct WorkPool<T, R> {
    pending: std::vec::IntoIter<T>,
    running: JoinSet<R>,
    jobs: usize,
}

impl<T: Send + 'static, R: Send + 'static> WorkPool<T, R> {
    fn new(work: Vec<T>, jobs: usize) -> Self {
        Self {
            pending: work.into_iter(),
            running: JoinSet::new(),
            jobs,
        }
    }

    async fn next<F, Fut>(&mut self, launch: F) -> Result<Option<R>, tokio::task::JoinError>
    where
        F: Clone + Fn(T) -> Fut,
        Fut: Future<Output = R> + Send + 'static,
    {
        while self.running.len() < self.jobs {
            let Some(work) = self.pending.next() else { break };
            self.running.spawn(launch.clone()(work));
        }
        self.running.join_next().await.transpose()
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct WorkKey {
    id: String,
    target: Target,
    provenance: String,
}

struct PlannedWork {
    id: String,
    key: WorkKey,
    benchmark: Arc<Benchmark>,
    case_index: usize,
    target: Target,
}

struct Work {
    key: WorkKey,
    benchmark: Arc<Benchmark>,
    case_index: usize,
    target: Target,
    prepared: execution::Prepared,
}

async fn prepare_work(work: PlannedWork) -> Result<Work, String> {
    let prepared = execution::prepare(&work.benchmark, work.case_index, work.target)
        .await
        .map_err(|error| error.to_string())?;
    Ok(Work {
        key: WorkKey {
            id: work.id,
            target: work.target,
            provenance: prepared.identity.clone(),
        },
        benchmark: work.benchmark,
        case_index: work.case_index,
        target: work.target,
        prepared,
    })
}

struct Completed {
    key: WorkKey,
    elapsed_ms: u64,
    result: execution::Result,
}

impl Completed {
    fn row(self) -> ledger::Row {
        let (status, output) = format_result(self.result);
        ledger::Row {
            key: self.key,
            status,
            elapsed_ms: self.elapsed_ms,
            output,
        }
    }
}

struct Statistics {
    minimum: u128,
    median: u128,
    p90: u128,
    p99: u128,
    maximum: u128,
}

impl Statistics {
    fn from_samples(samples: &mut [u128]) -> Self {
        samples.sort_unstable();
        Self {
            minimum: samples[0],
            median: Self::percentile(samples, 50),
            p90: Self::percentile(samples, 90),
            p99: Self::percentile(samples, 99),
            maximum: samples[samples.len() - 1],
        }
    }

    fn percentile(samples: &[u128], percent: usize) -> u128 {
        let rank = percent.saturating_mul(samples.len()).saturating_add(99) / 100;
        samples[rank.saturating_sub(1).min(samples.len() - 1)]
    }
}

struct Report;

impl Report {
    fn finish(rows: &std::collections::BTreeMap<WorkKey, ledger::Row>) -> Result<(), Error> {
        let mut passed = 0;
        let mut failures = Vec::new();
        for row in rows.values() {
            println!("{}", row.output);
            if row.status == "pass" {
                passed += 1;
            } else {
                failures.push(row.output.clone());
            }
        }
        println!("bench: {passed} passed; {} failed", failures.len());
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("\n").into())
        }
    }
}

fn format_result(result: execution::Result) -> (&'static str, String) {
    match result {
        execution::Result::Passed(passed) => {
            let id = passed.id.clone();
            let target = passed.provenance.target;
            let output = format_passed(passed);
            if output.len() <= EVIDENCE_LIMIT {
                ("pass", output)
            } else {
                (
                    "fail",
                    format!("FAIL bench/{id} {target}: formatted benchmark evidence exceeded {EVIDENCE_LIMIT} bytes"),
                )
            }
        }
        execution::Result::Failed(failed) => {
            let reason = excerpt(&failed.reason, DIAGNOSTIC_LIMIT);
            ("fail", format!("FAIL bench/{} {}: {reason}", failed.id, failed.target))
        }
    }
}

fn excerpt(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} ... [{} bytes omitted]", &value[..end], value.len() - end)
}

fn format_passed(mut passed: execution::Passed) -> String {
    let statistics = Statistics::from_samples(&mut passed.samples);
    let mut lines = vec![format!(
        "PASS bench/{} {} provenance={} cold_ms={} min_ms={} median_ms={} p90_ms={} p99_ms={} max_ms={} samples={} warmups={} image={:?} execution={:?}",
        passed.id,
        passed.provenance.target,
        passed.provenance.identity,
        passed.cold,
        statistics.minimum,
        statistics.median,
        statistics.p90,
        statistics.p99,
        statistics.maximum,
        passed.samples.len(),
        passed.provenance.warmups,
        passed.provenance.image,
        passed.provenance.execution,
    )];
    let setup = passed
        .setup
        .iter()
        .map(|(name, time)| format!("{name}_us={time}"))
        .collect::<Vec<_>>()
        .join(" ");
    lines.push(format!(
        "SETUP bench/{} {} {setup}",
        passed.id, passed.provenance.target
    ));
    let cold_lifecycle = passed
        .cold_lifecycle
        .iter()
        .map(|(name, time)| format!("{name}_us={time}"))
        .collect::<Vec<_>>()
        .join(" ");
    lines.push(format!(
        "COLD_LIFECYCLE bench/{} {} {cold_lifecycle}",
        passed.id, passed.provenance.target
    ));
    for (phase, mut times) in passed.lifecycle {
        let statistics = Statistics::from_samples(&mut times);
        lines.push(format!(
            "LIFECYCLE bench/{}/{phase} {} min_us={} median_us={} p90_us={} p99_us={} max_us={} samples={}",
            passed.id,
            passed.provenance.target,
            statistics.minimum,
            statistics.median,
            statistics.p90,
            statistics.p99,
            statistics.maximum,
            times.len(),
        ));
    }
    for (phase, mut times) in passed.phases {
        let statistics = Statistics::from_samples(&mut times);
        lines.push(format!(
            "PHASE bench/{}/{phase} {} min_us={} median_us={} p90_us={} p99_us={} max_us={} samples={}",
            passed.id,
            passed.provenance.target,
            statistics.minimum,
            statistics.median,
            statistics.p90,
            statistics.p99,
            statistics.maximum,
            times.len(),
        ));
    }
    lines.join("\n")
}

fn plan(benchmarks: Vec<Benchmark>, options: &Options) -> Vec<PlannedWork> {
    let mut work = Vec::new();
    for benchmark in benchmarks {
        let benchmark = Arc::new(benchmark);
        for target in options.targets() {
            for (case_index, case) in benchmark.cases.iter().enumerate() {
                let id = format!("bench/{}/{}", benchmark.name, case.id);
                work.push(PlannedWork {
                    id: id.clone(),
                    key: WorkKey {
                        id,
                        target,
                        provenance: String::new(),
                    },
                    benchmark: Arc::clone(&benchmark),
                    case_index,
                    target,
                });
            }
        }
    }
    work.sort_by(|left, right| left.key.cmp(&right.key));
    work
}

#[derive(Args)]
pub(crate) struct Options {
    /// Run only the named benchmark definition.
    name: Option<String>,
    /// Run only one guest ISA.
    #[arg(long = "isa", value_enum)]
    target: Option<Target>,
    /// Maximum concurrent rows; use 1 for uncontended latency comparisons.
    #[arg(long, env = "HL_BENCH_JOBS", default_value_t = logical_jobs(), value_parser = parse_jobs)]
    jobs: usize,
    /// Resume exact completed rows from the synchronized partial result.
    #[arg(long, env = "HL_BENCH_RESUME", default_value_t = false)]
    resume: bool,
    /// Relative durable result path beneath the repository workspace.
    #[arg(long, default_value = "target/testing/bench/results.tsv", value_parser = parse_results)]
    results: PathBuf,
}

fn logical_jobs() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(256)
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
    if value.is_empty() || path.is_absolute() || path.components().any(|part| matches!(part, Component::ParentDir)) {
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

    fn benchmarks(&self) -> Result<Vec<Benchmark>, Error> {
        let root = runtime::workspace()?.join("tests/bench");
        let mut directories = std::fs::read_dir(&root)?
            .map(|entry| entry.map(|value| value.path()))
            .collect::<Result<Vec<PathBuf>, _>>()?;
        directories.sort();
        let mut benchmarks = Vec::new();
        for directory in directories.into_iter().filter(|path| path.is_dir()) {
            let name = directory
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if self.name.as_deref().is_some_and(|wanted| wanted != name) {
                continue;
            }
            let definition = directory.join("test.yaml");
            if definition.is_file() {
                benchmarks.push(Benchmark::load(&directory, &definition)?);
            }
        }
        if benchmarks.is_empty() {
            Err(format!("no benchmarks matched under {}", root.display()).into())
        } else {
            Ok(benchmarks)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DIAGNOSTIC_LIMIT, Options, Statistics, WorkPool, excerpt, format_passed, parse_jobs, parse_results};
    use clap::Parser;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::{Duration, Instant};

    #[test]
    fn benchmark_evidence_separates_row_setup_from_execution() {
        let output = format_passed(super::execution::Passed {
            id: "startup/tiny".into(),
            cold: 12,
            samples: vec![4],
            phases: [("guest_compute".into(), vec![2])].into(),
            lifecycle: [("wait_and_drain".into(), vec![3])].into(),
            cold_lifecycle: [("start".into(), 5)].into(),
            setup: [("provenance_build".into(), 9)].into(),
            provenance: super::execution::Provenance {
                image: "alpine:3.20".into(),
                execution: "Native".into(),
                target: "arm64",
                warmups: 0,
                identity: "identity".into(),
            },
        });

        assert!(output.contains("SETUP bench/startup/tiny arm64 provenance_build_us=9"));
        assert!(output.contains("COLD_LIFECYCLE bench/startup/tiny arm64 start_us=5"));
        assert!(output.contains("LIFECYCLE bench/startup/tiny/wait_and_drain arm64 min_us=3"));
        assert!(output.contains("PHASE bench/startup/tiny/guest_compute arm64 min_us=2"));
    }

    #[derive(Parser)]
    struct BenchCli {
        #[command(flatten)]
        options: Options,
    }

    #[test]
    fn nearest_rank_percentiles_are_stable_for_small_samples() {
        let mut samples = [100, 1, 20, 3, 4];
        let summary = Statistics::from_samples(&mut samples);
        assert_eq!(summary.minimum, 1);
        assert_eq!(summary.median, 4);
        assert_eq!(summary.p90, 100);
        assert_eq!(summary.p99, 100);
        assert_eq!(summary.maximum, 100);
    }

    #[test]
    fn jobs_and_results_are_bounded() {
        assert_eq!(parse_jobs("1").unwrap(), 1);
        assert_eq!(parse_jobs("256").unwrap(), 256);
        assert!(parse_jobs("0").is_err());
        assert!(parse_jobs("257").is_err());
        assert!(parse_results("target/bench.tsv").is_ok());
        assert!(parse_results("../bench.tsv").is_err());
    }

    #[test]
    fn typed_cli_accepts_serial_mode_and_rejects_invalid_bounds() {
        let parsed = BenchCli::try_parse_from(["bench", "combined", "--jobs", "1", "--resume"]).unwrap();
        assert_eq!(parsed.options.jobs, 1);
        assert!(parsed.options.resume);
        assert!(BenchCli::try_parse_from(["bench", "--jobs", "0"]).is_err());
        assert!(BenchCli::try_parse_from(["bench", "--jobs", "257"]).is_err());
    }

    #[test]
    fn failure_excerpt_is_bounded_and_preserves_utf8() {
        let value = "é".repeat(DIAGNOSTIC_LIMIT);
        let excerpt = excerpt(&value, DIAGNOSTIC_LIMIT + 1);
        assert!(excerpt.is_char_boundary(excerpt.len()));
        assert!(excerpt.len() < DIAGNOSTIC_LIMIT + 128);
        assert!(excerpt.contains("bytes omitted"));
    }

    #[tokio::test]
    async fn work_pool_bounds_in_flight_rows_and_overlaps_work() {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let launch = {
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            move |ordinal| {
                let active = Arc::clone(&active);
                let maximum = Arc::clone(&maximum);
                async move {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(current, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    ordinal
                }
            }
        };
        let started = Instant::now();
        let mut pool = WorkPool::new((0..6).collect(), 2);
        let mut completed = Vec::new();
        while let Some(value) = pool.next(launch.clone()).await.unwrap() {
            completed.push(value);
        }
        completed.sort_unstable();
        assert_eq!(completed, (0..6).collect::<Vec<_>>());
        assert_eq!(maximum.load(Ordering::SeqCst), 2);
        assert!(started.elapsed() >= Duration::from_millis(55));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn timed_out_row_cleans_up_before_its_slot_is_replaced() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let launch = {
            let events = Arc::clone(&events);
            move |ordinal| {
                let events = Arc::clone(&events);
                async move {
                    events.lock().unwrap().push(format!("start{ordinal}"));
                    if ordinal == 0 {
                        let _ = tokio::time::timeout(Duration::from_millis(1), async {
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        })
                        .await;
                        events.lock().unwrap().push("cleanup0".to_owned());
                    }
                    events.lock().unwrap().push(format!("done{ordinal}"));
                }
            }
        };
        let mut pool = WorkPool::new(vec![0, 1], 1);
        while pool.next(launch.clone()).await.unwrap().is_some() {}
        assert_eq!(
            *events.lock().unwrap(),
            ["start0", "cleanup0", "done0", "start1", "done1"]
        );
    }
}
