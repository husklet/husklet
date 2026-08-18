use super::{
    Error,
    definition::{Sample, Scenario},
};
use crate::{
    runtime::image::TestImage,
    suite::{BoundedCapture as _, Capture, Target},
};
use hl_container::{Config, ContainerSpec, ExecSpec, ExitStatus, Stream};
use hl_images::RuntimeConfig;
use std::{fmt::Display, fs, future::Future, sync::Arc, time::Duration};
use tokio::time::Instant;

const IMAGE_TIMEOUT: Duration = Duration::from_secs(600);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);
const DIAGNOSTIC_LIMIT: usize = 4096;

pub enum CaseResult {
    Passed(String),
    Failed(String),
    ExpectedFailure(String),
    UnexpectedPass,
    NotRun(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PhaseTiming {
    pub setup_us: u64,
    pub execution_us: u64,
    pub payload_us: Option<u64>,
    pub teardown_us: u64,
    pub terminal_steps: Vec<super::terminal::Metric>,
}

pub struct CaseOutcome {
    pub result: CaseResult,
    pub timing: PhaseTiming,
}

impl CaseOutcome {
    pub(super) async fn run_on(
        provider: &Provider,
        scenario: Arc<Scenario>,
        case_index: usize,
        target: Target,
        sample: u16,
    ) -> Self {
        let case = &scenario.cases[case_index];
        let (outcome, timing) = execute_case(provider, &scenario, case, target, sample).await;
        Self {
            result: classify(outcome, case.expects_failure(target)),
            timing,
        }
    }
}

/// A provider service may be reused only after the preceding container was
/// force-removed. Each case still receives a fresh image view, container name,
/// process tree, and writable state.
pub(super) struct Provider {
    _state: tempfile::TempDir,
    containers: hl_container::Containers,
}

impl Provider {
    pub(super) async fn start() -> Result<Self, Error> {
        let state = tempfile::tempdir()?;
        let containers = hl_container::Containers::builder(Config::new(state.path()))
            .build()
            .await?;
        Ok(Self {
            _state: state,
            containers,
        })
    }
}

impl CaseResult {
    pub(super) fn evidence(&self) -> (&'static str, String) {
        match self {
            Self::Passed(evidence) => ("pass", evidence.clone()),
            Self::Failed(error) => ("fail", diagnostic(error)),
            Self::ExpectedFailure(error) => ("xfail", diagnostic(error)),
            Self::UnexpectedPass => ("xpass", "unexpected pass".to_owned()),
            Self::NotRun(reason) => ("skip", diagnostic(reason)),
        }
    }
}

fn classify(outcome: Result<String, Error>, expected_failure: bool) -> CaseResult {
    match (outcome, expected_failure) {
        (Ok(evidence), false) => CaseResult::Passed(evidence),
        (Err(error), true) => CaseResult::ExpectedFailure(error.to_string()),
        (Ok(_), true) => CaseResult::UnexpectedPass,
        (Err(error), false) => CaseResult::Failed(error.to_string()),
    }
}

async fn execute_case(
    provider: &Provider,
    scenario: &Scenario,
    case: &Sample,
    target: Target,
    sample: u16,
) -> (Result<String, Error>, PhaseTiming) {
    let setup = Instant::now();
    let mut timing = PhaseTiming::default();
    let result = execute_case_inner(provider, scenario, case, target, sample, &mut timing).await;
    if timing.setup_us == 0 {
        timing.setup_us = micros(setup.elapsed());
    }
    (result, timing)
}

async fn execute_case_inner(
    provider: &Provider,
    scenario: &Scenario,
    case: &Sample,
    target: Target,
    sample: u16,
    timing: &mut PhaseTiming,
) -> Result<String, Error> {
    validate_supported(case)?;
    let setup = Instant::now();
    let image = tokio::time::timeout(IMAGE_TIMEOUT, TestImage::materialize(&case.image, &target.platform()))
        .await
        .map_err(|_| {
            format!(
                "image {} did not materialize within {} seconds",
                case.image,
                IMAGE_TIMEOUT.as_secs()
            )
        })??;
    install_fixtures(case, image.path())?;
    timing.setup_us = micros(setup.elapsed());
    let outcome = execute_image(
        &provider.containers,
        scenario,
        case,
        target,
        sample,
        image.path(),
        image.runtime(),
        timing,
    )
    .await;
    let teardown = Instant::now();
    let release = image.release();
    timing.teardown_us = timing.teardown_us.saturating_add(micros(teardown.elapsed()));
    combine(
        outcome,
        release.map_err(|error| format!("image release failed: {error}")),
    )
}

fn validate_supported(case: &Sample) -> Result<(), Error> {
    let entrypoints = case
        .actions
        .iter()
        .filter(|action| matches!(action, super::definition::Step::Entrypoint))
        .count();
    if entrypoints != 0 && (case.actions.len() != 1 || case.readiness.is_some()) {
        return Err(format!(
            "{} entrypoint cannot be combined with ordered actions or readiness",
            case.id
        )
        .into());
    }
    Ok(())
}

async fn execute_image(
    containers: &hl_container::Containers,
    scenario: &Scenario,
    case: &Sample,
    target: Target,
    sample: u16,
    image: &std::path::Path,
    runtime: &RuntimeConfig,
    timing: &mut PhaseTiming,
) -> Result<String, Error> {
    let name = format!(
        "testing-{}-{}-{}-{sample}",
        scenario.name,
        target.name(),
        case.id.replace('/', "-")
    );
    let spec = specification(case, target, image, runtime, &name)?;
    let timeout = Duration::from_secs(case.timeout);
    let mut terminal_steps = Vec::new();
    let (action, cleanup) = execute_phases(
        timing,
        async {
            containers.create(spec).await?;
            containers.start(&name).await
        },
        || async {
            tokio::time::timeout(
                timeout,
                ActionOutput::execute(containers, case, runtime, image, &name, &mut terminal_steps),
            )
            .await
            .map_err(|_| format!("timed out after {} milliseconds", timeout.as_millis()))
            .and_then(|result| result.map_err(|error| error.to_string()))
        },
        async {
            tokio::time::timeout(CLEANUP_TIMEOUT, containers.remove_force(&name))
                .await
                .map_err(|_| format!("cleanup timed out after {} seconds", CLEANUP_TIMEOUT.as_secs()))
                .and_then(|result| result.map(|_| ()).map_err(|error| error.to_string()))
        },
    )
    .await;
    timing.terminal_steps = terminal_steps;
    let outcome = action.and_then(|output| {
        verify(case, output.status, &output.stdout, &output.stderr).map_err(|error| error.to_string())
    });
    combine(
        outcome.map_err(Into::into),
        cleanup.map_err(|error| format!("cleanup failed: {error}")),
    )
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

async fn measured<T>(slot: &mut u64, future: impl Future<Output = T>) -> T {
    let started = Instant::now();
    let output = future.await;
    *slot = slot.saturating_add(micros(started.elapsed()));
    output
}

async fn execute_phases<T, E, S, A, AF, C>(
    timing: &mut PhaseTiming,
    setup: S,
    action: A,
    cleanup: C,
) -> (Result<T, String>, Result<(), String>)
where
    E: Display,
    S: Future<Output = Result<(), E>>,
    A: FnOnce() -> AF,
    AF: Future<Output = Result<T, String>>,
    C: Future<Output = Result<(), String>>,
{
    let prepared = measured(&mut timing.setup_us, setup).await;
    let action = measured(&mut timing.execution_us, async {
        match prepared {
            Ok(()) => action().await,
            Err(error) => Err(error.to_string()),
        }
    })
    .await;
    let cleanup = measured(&mut timing.teardown_us, cleanup).await;
    (action, cleanup)
}

fn install_fixtures(case: &Sample, image: &std::path::Path) -> Result<(), Error> {
    for fixture in &case.fixtures {
        let destination = image.join(fixture.destination.trim_start_matches('/'));
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&fixture.source, destination)?;
    }
    Ok(())
}

fn specification(
    case: &Sample,
    target: Target,
    image: &std::path::Path,
    runtime: &RuntimeConfig,
    name: &str,
) -> Result<ContainerSpec, Error> {
    let process = super::process::initial(case, runtime, image)?;
    Ok(ContainerSpec::from_directory(image, process)
        .name(name)
        .guest(target.guest())
        .execution(case.execution.container()?)
        .isolation(super::isolation::for_case(case)))
}

/// Runs the case's startup command, then polls its probe until the service answers.
///
/// The probe is what decides readiness, so a failing startup is reported as its own failure
/// rather than as a timeout, and an exhausted poll quotes the service's own logs.
async fn await_readiness(
    containers: &hl_container::Containers,
    case: &Sample,
    runtime: &RuntimeConfig,
    rootfs: &std::path::Path,
    name: &str,
    readiness: &super::definition::Readiness,
) -> Result<(), Error> {
    let startup = super::definition::Step::Shell(readiness.startup.clone());
    let startup = run_exec(containers, case, runtime, rootfs, name, &startup).await?;
    require_success("readiness startup", startup.0, &startup.1, &startup.2)?;
    let probe = super::definition::Step::Shell(readiness.probe.clone());
    for attempt in 1..=readiness.attempts {
        if run_exec(containers, case, runtime, rootfs, name, &probe).await?.0 == ExitStatus::Code(0) {
            return Ok(());
        }
        if attempt < readiness.attempts {
            tokio::time::sleep(Duration::from_millis(readiness.delay_ms)).await;
        }
    }
    Err(format!(
        "readiness failed after {} attempts; service logs: {}",
        readiness.attempts,
        readiness_logs(rootfs, &readiness.logs)
    )
    .into())
}

impl ActionOutput {
    async fn execute(
        containers: &hl_container::Containers,
        case: &Sample,
        runtime: &RuntimeConfig,
        rootfs: &std::path::Path,
        name: &str,
        terminal_metrics: &mut Vec<super::terminal::Metric>,
    ) -> Result<Self, Error> {
        if matches!(case.actions.first(), Some(super::definition::Step::Entrypoint)) {
            let status = wait(containers, name, Duration::from_secs(case.timeout)).await?;
            let logs = containers.logs(name).await?;
            logs.bounded()?;
            return Ok(Self {
                status,
                stdout: logs.stdout,
                stderr: logs.stderr,
            });
        }
        if let Some(readiness) = &case.readiness {
            await_readiness(containers, case, runtime, rootfs, name, readiness).await?;
        }
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut status = ExitStatus::Code(0);
        for action in &case.actions {
            let outcome = match action {
                super::definition::Step::Api(operation) => run_api(containers, name, operation).await?,
                super::definition::Step::Terminal(action) => {
                    super::terminal::run(containers, case, runtime, rootfs, name, action, terminal_metrics).await?
                }
                _ => run_exec(containers, case, runtime, rootfs, name, action).await?,
            };
            status = outcome.0;
            stdout.extend(outcome.1);
            stderr.extend(outcome.2);
            crate::suite::Capture::bounded(stdout.len(), stderr.len())?;
            if status != ExitStatus::Code(0) {
                break;
            }
        }
        Ok(Self { status, stdout, stderr })
    }
}

struct ActionOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn run_api(
    containers: &hl_container::Containers,
    name: &str,
    operation: &super::definition::ApiStep,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), Error> {
    use super::definition::ApiStep;
    let filesystem = containers.filesystem(name).await?;
    match operation {
        ApiStep::CopyToContainer { source, destination } => {
            let mut bytes = LimitedOutput::default();
            {
                let mut archive = tar::Builder::new(&mut bytes);
                let entry = source.file_name().ok_or("copy source has no basename")?;
                if source.is_dir() {
                    archive.append_dir_all(entry, source)?;
                } else {
                    archive.append_path_with_name(source, entry)?;
                }
                archive.finish()?;
            }
            let bytes = bytes.0;
            filesystem.extract(
                destination,
                bytes.as_slice(),
                hl_container::Limits {
                    entries: 1024,
                    bytes: Capture::LIMIT as u64,
                },
            )?;
            Ok((ExitStatus::Code(0), Vec::new(), Vec::new()))
        }
        ApiStep::CopyFromContainer { source } => {
            let mut archive = LimitedOutput::default();
            filesystem.archive(source, &mut archive)?;
            let output = unpack_regular_files(&archive.0)?;
            Ok((ExitStatus::Code(0), output, Vec::new()))
        }
    }
}

mod output;
use output::{
    LimitedOutput, combine, diagnostic, readiness_logs, require_success, run_exec, unpack_regular_files, verify, wait,
};
#[cfg(test)]
mod test;
