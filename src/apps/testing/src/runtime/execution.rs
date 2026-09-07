#[path = "failure_retention.rs"]
mod retention;
#[path = "execution_worker.rs"]
mod worker;

use super::diagnostic::Excerpt as _;
use super::{
    Error,
    definition::App,
    image::{Materialization, TestImage},
    output,
};
use crate::suite::{BoundedCapture as _, Target};
use hl_container::{Config, Console, ContainerSpec, Containers, ExitStatus, Isolation, Mount, Process, Sandbox, Size};
use retention::FailureRetention;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path, sync::Arc, time::Duration};
use tokio::time::Instant;

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) use worker::Options as WorkerOptions;

#[derive(Clone, Deserialize, Serialize)]
pub enum CaseResult {
    Passed(String, Option<u16>),
    Failed(String, Option<u16>, String),
}

impl CaseResult {
    pub(crate) const fn passed(&self) -> bool {
        matches!(self, Self::Passed(_, _))
    }

    pub(crate) fn diagnostic(&self) -> Option<String> {
        match self {
            Self::Failed(_, attempt, error) => {
                Some(attempt.map_or_else(|| error.clone(), |value| format!("attempt {value}: {error}")))
            }
            Self::Passed(_, _) => None,
        }
    }
}

pub struct Report {
    pub results: Vec<CaseResult>,
    /// One `native counter=value ...` line, empty when the app does not emit diagnostics.
    pub counters: String,
    pub measurement: Option<EngineMeasurement>,
}

pub struct EngineMeasurement {
    pub case: String,
    pub target: Target,
    pub wall_ns: u64,
    pub raw: String,
}
pub async fn run_case(
    app: Arc<App>,
    case_index: usize,
    target: Target,
    allow_broken: bool,
    engine_measurement: Option<std::path::PathBuf>,
) -> Result<Report, Error> {
    let case = &app.cases[case_index];
    worker::run(
        &app.name,
        &case.id,
        target,
        case.declared_timeout(),
        &case.diagnostics,
        allow_broken,
        engine_measurement,
    )
    .await
}

pub(crate) async fn worker(options: WorkerOptions) -> Result<(), Error> {
    worker::execute(options).await
}

async fn run_case_inner(
    app: Arc<App>,
    case_index: usize,
    target: Target,
    retention: Option<FailureRetention>,
    engine_measurement: Option<std::path::PathBuf>,
) -> Result<Vec<CaseResult>, Error> {
    let execution = app.execution.container()?;
    if let Some(unwired) = app.cases[case_index].engine_options.unwired() {
        return Err(unwired.into());
    }
    let building = Arc::clone(&app);
    let artifact = tokio::task::spawn_blocking(move || {
        building
            .build(&building.cases[case_index], target)
            .map_err(|error| error.to_string())
    })
    .await??;
    let case = &app.cases[case_index];
    let mode = if case.engine_options.mounts().iter().any(|mount| mount.populate) {
        Materialization::Copy
    } else {
        Materialization::from_environment()
    };
    let mut fixture = materialize(&app, case, target, mode).await?;
    let work_root = super::work_root::WorkRoot::open()?;
    let state = tokio::task::spawn_blocking(move || work_root.temporary_state()).await??;

    let mut config = Config::new(state.path());
    if let Some(cache) = case.engine_options.translation_cache() {
        config = config.translation_cache(cache);
    }
    if let Some(enabled) = case.engine_options.direct_call_pre_spill() {
        config = config.direct_call_pre_spill(enabled);
    }
    let containers = hl_container::Containers::builder(config)
        .images(fixture.images())
        .build()
        .await?;
    let results = CaseExecution::new(
        &app,
        case,
        target,
        &containers,
        execution,
        retention.as_ref(),
        engine_measurement,
    )
    .run(&mut fixture, artifact.path())
    .await;
    fixture.release()?;
    Ok(results)
}

async fn materialize(
    app: &App,
    case: &super::definition::Workload,
    target: Target,
    mode: Materialization,
) -> Result<TestImage, Error> {
    let fixture = match case.rootfs {
        super::definition::Rootfs::Image => TestImage::materialize_with(&app.image, &target.platform(), mode)
            .await
            .map_err(|error| format!("materialize image {} for {}: {error}", app.image, target.name()))?,
        super::definition::Rootfs::Scratch => TestImage::materialize_scratch(&target.platform(), mode)
            .map_err(|error| format!("materialize scratch root for {}: {error}", target.name()))?,
    };
    Ok(fixture)
}

async fn stage(
    fixture: &TestImage,
    case: &super::definition::Workload,
    artifact: &Path,
    target: Target,
) -> Result<(), Error> {
    let root = fixture.path();
    let destination = root.join(case.destination.trim_start_matches('/'));
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| context("create staging directory", parent, &error))?;
    }
    tokio::fs::copy(artifact, &destination)
        .await
        .map_err(|error| format!("stage {} into {}: {error}", artifact.display(), destination.display()))?;
    make_executable(&destination).map_err(|error| context("make executable", &destination, &error))?;
    provision(root, fixture.lower(), case, target).await
}

fn assert_overlay(fixture: &TestImage, case: &super::definition::Workload) -> Result<(), Error> {
    let Some(lower) = fixture.lower() else {
        return Ok(());
    };
    let relative = case.destination.trim_start_matches('/');
    let upper = fixture.path().join(relative);
    if !upper.exists() {
        return Err(format!(
            "overlay proof: {} is not in the upper {}",
            relative,
            fixture.path().display()
        )
        .into());
    }
    if lower.join(relative).exists() {
        return Err(format!("overlay proof: {relative} is already in the lower {}", lower.display()).into());
    }
    if fixture.reference().overlay().is_none() {
        return Err("overlay proof: rootfs reference carries no lower/upper split"
            .to_owned()
            .into());
    }
    Ok(())
}

async fn provision(
    root: &std::path::Path,
    lower: Option<&std::path::Path>,
    case: &super::definition::Workload,
    target: Target,
) -> Result<(), Error> {
    // A dynamically linked case needs its PT_INTERP loader and shared libraries, which the base
    // image's libc does not supply; they come from the same cross toolchain that built the binary.
    for library in &case.guest_libraries {
        let (host, path) = (
            library.host(target),
            root.join(library.guest(target).trim_start_matches('/')),
        );
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| context("create guest library directory", parent, &error))?;
        }
        tokio::fs::copy(host, &path)
            .await
            .map_err(|error| format!("stage guest library {host} into {}: {error}", path.display()))?;
        make_executable(&path).map_err(|error| context("make guest library executable", &path, &error))?;
    }
    for file in &case.guest_files {
        let path = root.join(file.path().trim_start_matches('/'));
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| context("create guest file directory", parent, &error))?;
        }
        tokio::fs::write(&path, file.contents())
            .await
            .map_err(|error| context("write guest file", &path, &error))?;
        set_private(&path).map_err(|error| context("set guest file mode", &path, &error))?;
    }
    for elf in &case.guest_elf {
        let relative = elf.path().trim_start_matches('/');
        let upper = root.join(relative);
        let path = if upper.exists() {
            upper
        } else {
            lower.map_or(upper, |base| base.join(relative))
        };
        super::definition::elf::verify(&path, target, elf.expectation())?;
    }
    if let Some(cwd) = &case.working_directory {
        let path = root.join(cwd.trim_start_matches('/'));
        tokio::fs::create_dir_all(&path)
            .await
            .map_err(|error| context("create guest working directory", &path, &error))?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_private(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(windows)]
fn set_private(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

fn context(operation: &str, path: &std::path::Path, error: &std::io::Error) -> String {
    format!("{operation} {}: {error}", path.display())
}

#[cfg(unix)]
fn make_executable(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[cfg(windows)]
fn make_executable(_path: &std::path::Path) -> std::io::Result<()> {
    // Windows has no executable permission bit. The guest loader consumes the
    // staged Linux image as bytes, so copying it completed this host-side step.
    Ok(())
}

struct CaseExecution<'a> {
    app: &'a App,
    case: &'a super::definition::Workload,
    target: Target,
    containers: &'a Containers,
    execution: hl_container::Execution,
    retention: Option<&'a FailureRetention>,
    engine_measurement: Option<std::path::PathBuf>,
}

impl<'a> CaseExecution<'a> {
    fn new(
        app: &'a App,
        case: &'a super::definition::Workload,
        target: Target,
        containers: &'a Containers,
        execution: hl_container::Execution,
        retention: Option<&'a FailureRetention>,
        engine_measurement: Option<std::path::PathBuf>,
    ) -> Self {
        if let Some(plan) = &case.soak {
            let resources = plan.resources();
            println!(
                "SOAK {} {} attempts={} duration={}s resources=cpu:{},memory_mib:{},processes:{} (admission only)",
                case.id,
                target.name(),
                plan.repetitions(),
                plan.duration().as_secs(),
                resources.cpu(),
                resources.memory_mib(),
                resources.processes()
            );
        }
        Self {
            app,
            case,
            target,
            containers,
            execution,
            retention,
            engine_measurement,
        }
    }

    async fn run(&self, fixture: &mut TestImage, artifact: &Path) -> Vec<CaseResult> {
        let Some(plan) = &self.case.soak else {
            let timeout = Duration::from_secs(self.case.timeout);
            return vec![self.staged_attempt(fixture, artifact, 1, 1, timeout, false).await];
        };
        let end = Instant::now() + plan.total_duration();
        let mut results = Vec::with_capacity(plan.attempts().len());
        for attempt in plan.attempts() {
            let remaining = end.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                results.push(CaseResult::Failed(
                    self.case.id.clone(),
                    Some(attempt.ordinal()),
                    "total soak deadline expired before launch".to_owned(),
                ));
                break;
            }
            results.push(
                self.staged_attempt(
                    fixture,
                    artifact,
                    attempt.ordinal(),
                    plan.repetitions(),
                    plan.duration().min(remaining),
                    attempt.ordinal() > 1,
                )
                .await,
            );
        }
        results
    }

    /// Container removal consumes an image-backed rootfs, so every attempt after
    /// the first gets a fresh writable root and is staged into it again.
    async fn staged_attempt(
        &self,
        fixture: &mut TestImage,
        artifact: &Path,
        ordinal: u16,
        repetitions: u16,
        timeout: Duration,
        refork: bool,
    ) -> CaseResult {
        let attempt = (repetitions > 1).then_some(ordinal);
        let prepared = async {
            if refork {
                fixture.refork()?;
            }
            stage(fixture, self.case, artifact, self.target).await?;
            assert_overlay(fixture, self.case)
        }
        .await;
        if let Err(error) = prepared {
            return CaseResult::Failed(self.case.id.clone(), attempt, error.to_string());
        }
        self.attempt(fixture, artifact, ordinal, repetitions, timeout).await
    }

    async fn attempt(
        &self,
        fixture: &TestImage,
        artifact: &Path,
        ordinal: u16,
        repetitions: u16,
        timeout: Duration,
    ) -> CaseResult {
        let attempt = (repetitions > 1).then_some(ordinal);
        let name = format!(
            "testing-{}-{}-{}-{ordinal}",
            self.app.name,
            self.target.name(),
            self.case.id.replace('/', "-")
        );
        let mut process = Process::new(&self.case.destination).args(self.case.arguments.iter().map(String::as_str));
        let checkpoint_cycles = self
            .case
            .orchestration
            .and_then(|orchestration| orchestration.container_checkpoint_cycles);
        if checkpoint_cycles.is_some() {
            process = process.console(Console {
                stdin: false,
                terminal: Some(Size::new(24, 80).expect("fixed checkpoint fixture terminal")),
            });
        }
        if let Some(cwd) = &self.case.working_directory {
            process = process.working_dir(cwd);
        }
        for entry in &self.case.environment {
            process = process.env_bytes(entry.name().to_vec(), entry.value().to_vec());
        }
        let options = &self.case.engine_options;
        if let Some((uid, gid)) = options.user() {
            process = process.user(uid, gid);
        }
        let mut spec = ContainerSpec::new(fixture.reference().clone(), process)
            .name(&name)
            .guest(self.target.guest())
            .execution(self.execution)
            .isolation(options.isolation(Isolation {
                sandbox: Sandbox::Disabled,
                ..Isolation::default()
            }))
            .network_mode(options.network_mode())
            .resources(options.resources());
        if let Some(path) = &self.engine_measurement {
            spec = spec.benchmark_measurement(path);
        }
        if let Some(hostname) = options.hostname() {
            spec = spec.hostname(hostname);
        }
        for mount in options.mounts() {
            spec = spec.mount(mount.clone());
        }
        let checkpoint_state = checkpoint_cycles
            .map(|_| tempfile::Builder::new().prefix("container-checkpoint-").tempdir())
            .transpose();
        let checkpoint_state = match checkpoint_state {
            Ok(value) => value,
            Err(error) => return CaseResult::Failed(self.case.id.clone(), attempt, error.to_string()),
        };
        if let Some(state) = &checkpoint_state {
            spec = spec.backend_diagnostic(state.path().join("backend-diagnostics"));
            spec = spec.mount(Mount::read_write(state.path(), "/checkpoint-state"));
        }
        let mut status = None;
        let outcome = self
            .execute(spec, &name, timeout, &mut status, checkpoint_state.as_ref())
            .await
            .map_err(|error| error.to_string());
        let retained = if outcome.is_err() {
            self.retention.and_then(|retention| {
                retention
                    .retain(fixture, artifact, &self.case.id, self.target, ordinal, status)
                    .map_err(|error| eprintln!("failed to retain overlay for {}: {error}", self.case.id))
                    .ok()
            })
        } else {
            None
        };
        let cleanup = tokio::time::timeout(CLEANUP_TIMEOUT, self.containers.remove_force(&name)).await;
        match (outcome, cleanup) {
            (Ok(()), Ok(Ok(_))) => CaseResult::Passed(self.case.id.clone(), attempt),
            (Err(mut error), _) => {
                if let Some(path) = retained {
                    error.push_str(&format!("; retained_overlay={}", path.display()));
                }
                CaseResult::Failed(self.case.id.clone(), attempt, error)
            }
            (Ok(()), Ok(Err(error))) => {
                CaseResult::Failed(self.case.id.clone(), attempt, format!("cleanup failed: {error}"))
            }
            (Ok(()), Err(_)) => CaseResult::Failed(
                self.case.id.clone(),
                attempt,
                cleanup_timeout_diagnostic(CLEANUP_TIMEOUT),
            ),
        }
    }

    async fn execute(
        &self,
        spec: ContainerSpec,
        name: &str,
        timeout: Duration,
        observed: &mut Option<ExitStatus>,
        checkpoint_state: Option<&tempfile::TempDir>,
    ) -> Result<(), Error> {
        self.containers.create(spec).await?;
        if let Some((network, endpoint)) = self.case.engine_options.bridge()? {
            let networks = self.containers.networks();
            let created = networks.create(network).await?;
            networks.connect(&created.name, name, endpoint).await?;
        }
        if let Some(state) = checkpoint_state {
            return self
                .container_checkpoint_cycles(name, timeout, observed, state.path())
                .await;
        }
        self.containers.start(name).await?;
        let status = if let Some(orchestration) = self.case.orchestration {
            let delay = Duration::from_millis(orchestration.stop_after_ms.expect("validated stop orchestration"));
            tokio::time::sleep(delay).await;
            // The manifest timeout bounds the complete attempt, including the pre-stop delay.
            // Validation proves the delay is shorter, so this never silently widens the case.
            self.containers.stop(name, timeout.saturating_sub(delay)).await?
        } else {
            self.wait(name, timeout).await?
        };
        *observed = Some(status);
        let measured = self.containers.take_benchmark_measurement(name).await?;
        match (&self.engine_measurement, measured) {
            (Some(expected), Some(actual)) if actual.path == *expected && !actual.raw.is_empty() => {}
            (Some(_), None) => return Err("engine measurement was requested but omitted".into()),
            (None, Some(_)) => return Err("engine emitted an unrequested measurement".into()),
            (Some(expected), Some(actual)) => {
                return Err(format!(
                    "engine measurement identity mismatched: expected={} actual={}",
                    expected.display(),
                    actual.path.display()
                )
                .into());
            }
            (None, None) => {}
        }
        let mut logs = self.containers.logs(name).await?;
        logs.bounded()?;
        let mut profile_validation = output::validate_backend_tree(&logs.stderr, self.execution.diagnostics());
        let aarch64_interpreter_product = self.execution.diagnostics()
            && self.execution.is_translated()
            && self.target == Target::Arm64
            && cfg!(target_arch = "x86_64");
        profile_validation = profile_validation.and_then(|()| {
            output::aarch64_opcode_product(&logs.stderr, aarch64_interpreter_product, false, true).map(|_| ())
        });
        if self.execution.is_translated() && !aarch64_interpreter_product {
            profile_validation = profile_validation.and_then(|()| output::validate_translated_execution(&logs.stderr));
        }
        if self.execution.diagnostics() {
            let text = std::str::from_utf8(&logs.stderr).map_err(|_| "retained C diagnostics are not UTF-8")?;
            // Preserve a missing profile as a failure for otherwise-correct diagnostic runs, but do not let
            // secondary telemetry loss hide the guest's exit or output failure. Abnormal engine paths can end
            // before the retained dispatcher emits its summary, and the underlying compatibility defect is the
            // actionable diagnostic in that case.
            // An explicitly orchestrated signal ends the engine before its normal dispatcher
            // epilogue. The lifecycle result is the contract for that typed path; all ordinary
            // exits still require the complete profile summary.
            let retained_profile = self
                .case
                .expected_signal
                .map_or_else(|| output::validate_profile_or_product(text.as_bytes()), |_| Ok(()));
            profile_validation = profile_validation.and(retained_profile);
            output::forward_profile(text, std::io::stderr().lock())?;
            logs.stderr = output::guest_stderr(text);
        }
        let expected = if let Some(golden) = &self.case.golden {
            tokio::fs::read(golden)
                .await
                .map_err(|error| context("read golden", golden, &error))?
        } else {
            Vec::new()
        };
        super::outcome::validate(
            status,
            self.case.exit,
            self.case.expected_signal,
            &logs,
            &expected,
            &self.case.stderr,
            profile_validation,
        )
    }

    /// Two checkpoint/start cycles against one persisted container. This deliberately does not model
    /// workspace Continue-later, whose domain process is killed and reopened by a separate product journey.
    async fn container_checkpoint_cycles(
        &self,
        name: &str,
        timeout: Duration,
        observed: &mut Option<ExitStatus>,
        state: &Path,
    ) -> Result<(), Error> {
        let deadline = Instant::now() + timeout;
        let output = state.join("output");
        let diagnostics = state.join("backend-diagnostics");
        let mut diagnostic_offset = 0;
        let direct_call_guard = self
            .case
            .engine_options
            .direct_call_pre_spill()
            .filter(|_| self.target == Target::Amd64);
        bounded_checkpoint_phase(deadline, "initial container start", self.containers.start(name)).await?;
        for (generation, (marker, cycle)) in [("READY leader=", "cycle1"), ("CYCLE 1 progress=", "cycle2")]
            .into_iter()
            .enumerate()
        {
            wait_for_marker(&output, marker, deadline).await?;
            if let Err(error) = bounded_checkpoint_phase(
                deadline,
                "container checkpoint",
                self.containers.checkpoint(name, remaining(deadline)?),
            )
            .await
            {
                let logs = self.containers.logs(name).await;
                return Err(checkpoint_failure_diagnostic(&error.to_string(), logs.as_ref().ok()).into());
            }
            let generation_diagnostics = checkpoint_generation_diagnostics(&diagnostics, &mut diagnostic_offset)?;
            validate_checkpoint_generation(&generation_diagnostics, generation, self.target, direct_call_guard)?;
            std::fs::write(state.join(cycle), [])?;
            bounded_checkpoint_phase(deadline, "container restore", self.containers.start(name)).await?;
        }
        wait_for_marker(&output, "CYCLE 2 progress=", deadline).await?;
        std::fs::write(state.join("stop"), [])?;
        let status = bounded_checkpoint_phase(
            deadline,
            "final restored container exit",
            self.wait(name, remaining(deadline)?),
        )
        .await?;
        *observed = Some(status);
        let generation_diagnostics = checkpoint_generation_diagnostics(&diagnostics, &mut diagnostic_offset)?;
        validate_checkpoint_generation(&generation_diagnostics, 2, self.target, direct_call_guard)?;
        let text = std::fs::read_to_string(output)?;
        validate_daily_dev_protocol(&text)?;
        if status != ExitStatus::Code(0) {
            return Err(format!("container checkpoint fixture exited as {status:?}").into());
        }
        Ok(())
    }
}

fn checkpoint_failure_diagnostic(error: &str, logs: Option<&hl_container::Logs>) -> String {
    logs.map_or_else(
        || format!("container checkpoint failed: {error}; current container logs unavailable"),
        |logs| {
            format!(
                "container checkpoint failed: {error}; stderr={}; stdout={}",
                checkpoint_log_excerpt(&logs.stderr),
                checkpoint_log_excerpt(&logs.stdout)
            )
        },
    )
}

fn checkpoint_log_excerpt(bytes: &[u8]) -> String {
    const TAIL_BYTES: usize = 320;
    format!(
        "head={}; tail={}",
        bytes.preview(),
        bytes.escaped_from(bytes.len().saturating_sub(TAIL_BYTES))
    )
}

async fn bounded_checkpoint_phase<T, E>(
    deadline: Instant,
    phase: &str,
    operation: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, Error>
where
    E: Into<Error>,
{
    tokio::time::timeout_at(deadline, operation)
        .await
        .map_err(|_| format!("{phase} exceeded the container checkpoint case deadline"))?
        .map_err(Into::into)
}

fn remaining(deadline: Instant) -> Result<Duration, Error> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    (!remaining.is_zero())
        .then_some(remaining)
        .ok_or_else(|| "container checkpoint orchestration exceeded its case timeout".into())
}

async fn wait_for_marker(path: &Path, marker: &str, deadline: Instant) -> Result<(), Error> {
    loop {
        if tokio::fs::read_to_string(path)
            .await
            .is_ok_and(|text| text.contains(marker))
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("container checkpoint marker {marker:?} was not emitted").into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn checkpoint_generation_diagnostics(path: &Path, offset: &mut usize) -> Result<Vec<u8>, Error> {
    let diagnostics = std::fs::read(path)?;
    let generation = diagnostics
        .get(*offset..)
        .ok_or("checkpoint backend diagnostics shrank between generations")?
        .to_vec();
    *offset = diagnostics.len();
    Ok(generation)
}

fn validate_checkpoint_generation(
    generation: &[u8],
    ordinal: usize,
    target: Target,
    direct_call_guard: Option<bool>,
) -> Result<(), Error> {
    output::validate_backend_tree(generation, true)
        .map_err(|error| format!("generation {ordinal}: {error}; diagnostics={}", generation.preview()))?;
    if let Some(expected) = direct_call_guard {
        output::validate_direct_call_guard_candidate(generation, expected)
            .map_err(|error| format!("generation {ordinal}: {error}; diagnostics={}", generation.preview()))?;
    }
    let aarch64_interpreter_product = target == Target::Arm64 && cfg!(target_arch = "x86_64");
    output::aarch64_opcode_product(generation, aarch64_interpreter_product, true, true)
        .map_err(|error| format!("generation {ordinal}: {error}; diagnostics={}", generation.preview()))?;
    if !aarch64_interpreter_product {
        output::validate_translated_execution(generation)
            .map_err(|error| format!("generation {ordinal}: {error}; diagnostics={}", generation.preview()))?;
    }
    if !aarch64_interpreter_product {
        output::validate_profile_or_product(generation)
            .map_err(|error| format!("generation {ordinal}: {error}; diagnostics={}", generation.preview()))?;
    }
    let receipt = output::backend_execution_digest(generation);
    if receipt.is_empty() {
        return Err("validated checkpoint generation has no backend-tree receipt".into());
    }
    eprintln!("checkpoint-generation={ordinal} {receipt}");
    Ok(())
}

fn validate_daily_dev_protocol(text: &str) -> Result<(), Error> {
    for marker in [
        "READY leader=",
        "SLEEP-READY ",
        "CYCLE 1 ",
        "CYCLE 2 ",
        "DONE progress=",
        "JCC-LINK phase=0 value=42",
        "JCC-LINK phase=1 value=43",
        "JCC-LINK phase=2 value=44",
        "MIXED-SSE phase=0 value=42",
        "MIXED-SSE phase=1 value=43",
        "MIXED-SSE phase=2 value=44",
    ] {
        if text.matches(marker).count() != 1 {
            return Err(format!("daily-development checkpoint marker {marker:?} was not emitted exactly once").into());
        }
    }
    if text.contains("SLEEP-RETURN") || text.contains("IDENTITY-ERROR") {
        return Err("daily-development checkpoint process identity was not preserved".into());
    }
    validate_ret_dense_protocol(text)?;
    Ok(())
}

fn ret_dense_digest(chunks: u64) -> Result<u64, Error> {
    const DEPTH: u64 = 32;
    const CALLS: u64 = 4096;
    const STEP: u64 = 0x9e37_79b9_7f4a_7c15;
    const MIX: u64 = 0xd1b5_4a32_d192_ed03;
    if chunks > 10_000 {
        return Err("daily-development RET-DENSE chunk count exceeds the bounded validator".into());
    }
    let returned = STEP.wrapping_mul(DEPTH).wrapping_add(DEPTH * (DEPTH + 1) / 2);
    let mut digest = 0_u64;
    for chunk in 1..=chunks {
        for call in 0..CALLS {
            let value = (chunk << 32 | call).wrapping_add(returned);
            digest = (digest ^ value).rotate_left(7).wrapping_add(MIX);
        }
    }
    Ok(digest)
}

fn validate_ret_dense_protocol(text: &str) -> Result<(), Error> {
    let mut previous_chunks = 0;
    for phase in 0..=2 {
        let prefix = format!("RET-DENSE phase={phase} ");
        let rows = text
            .lines()
            .filter(|line| line.starts_with(&prefix))
            .collect::<Vec<_>>();
        if rows.len() != 1 {
            return Err(format!("daily-development {prefix:?}was not emitted exactly once").into());
        }
        let fields = rows[0]
            .split_ascii_whitespace()
            .skip(2)
            .filter_map(|field| field.split_once('='))
            .collect::<std::collections::BTreeMap<_, _>>();
        if fields.keys().copied().collect::<Vec<_>>() != ["chunks", "digest"] {
            return Err(format!("daily-development RET-DENSE schema mismatch: {}", rows[0]).into());
        }
        let chunks = fields["chunks"]
            .parse::<u64>()
            .map_err(|_| format!("daily-development RET-DENSE chunks are not decimal: {}", rows[0]))?;
        let digest = u64::from_str_radix(fields["digest"], 16)
            .map_err(|_| format!("daily-development RET-DENSE digest is not hexadecimal: {}", rows[0]))?;
        if chunks <= previous_chunks {
            return Err("daily-development RET-DENSE progress did not survive checkpoint restore".into());
        }
        if digest != ret_dense_digest(chunks)? {
            return Err(format!("daily-development RET-DENSE digest mismatch at phase {phase}").into());
        }
        previous_chunks = chunks;
    }
    Ok(())
}

#[cfg(test)]
mod checkpoint_protocol_tests {
    use super::{checkpoint_failure_diagnostic, checkpoint_generation_diagnostics, validate_daily_dev_protocol};

    fn complete() -> String {
        let rows = (0..=2)
            .map(|phase| {
                let chunks = phase + 1;
                format!(
                    "RET-DENSE phase={phase} chunks={chunks} digest={:016x}\n",
                    super::ret_dense_digest(chunks).unwrap()
                )
            })
            .collect::<String>();
        format!(
            "READY leader=1\nSLEEP-READY pid=2\nCYCLE 1 progress=5\nCYCLE 2 progress=10\n\
             DONE progress=11\nJCC-LINK phase=0 value=42\nJCC-LINK phase=1 value=43\n\
             JCC-LINK phase=2 value=44\nMIXED-SSE phase=0 value=42\nMIXED-SSE phase=1 value=43\n\
             MIXED-SSE phase=2 value=44\n{rows}"
        )
    }

    #[test]
    fn daily_development_protocol_requires_each_generation_exactly_once() {
        let complete = complete();
        validate_daily_dev_protocol(&complete).unwrap();
        assert!(validate_daily_dev_protocol(&format!("{complete}CYCLE 1 progress=6\n")).is_err());
        assert!(validate_daily_dev_protocol(&complete.replace("DONE progress=11\n", "")).is_err());
        assert!(validate_daily_dev_protocol(&format!("{complete}IDENTITY-ERROR\n")).is_err());
    }

    #[test]
    fn daily_development_protocol_rejects_reset_or_corrupt_return_digest() {
        let complete = complete();
        assert!(validate_daily_dev_protocol(&complete.replacen("chunks=2", "chunks=1", 1)).is_err());
        let digest = complete.find("digest=").unwrap() + "digest=".len();
        let mut corrupt = complete.clone();
        let replacement = if &corrupt[digest..digest + 1] == "0" { "1" } else { "0" };
        corrupt.replace_range(digest..digest + 1, replacement);
        assert!(validate_daily_dev_protocol(&corrupt).is_err());
    }

    #[test]
    fn checkpoint_generation_slices_the_private_backend_channel_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("backend-diagnostics");
        let mut offset = 0;
        std::fs::write(&path, b"backend-generation-0\n").unwrap();
        let first = checkpoint_generation_diagnostics(&path, &mut offset).unwrap();
        assert_eq!(first, b"backend-generation-0\n");

        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"backend-generation-1\n")
            .unwrap();
        let second = checkpoint_generation_diagnostics(&path, &mut offset).unwrap();
        assert_eq!(second, b"backend-generation-1\n");
    }

    #[test]
    fn checkpoint_failure_preserves_cause_and_bounded_current_logs() {
        let logs = hl_container::Logs {
            stdout: [
                b"stdout-early-marker:".as_slice(),
                &vec![b'o'; 8_192],
                b":stdout-late-checkpoint-cause",
            ]
            .concat(),
            stderr: [
                b"stderr-early-marker:".as_slice(),
                &vec![b'e'; 8_192],
                b":stderr-late-checkpoint-phase",
            ]
            .concat(),
        };
        let diagnostic = checkpoint_failure_diagnostic("engine refused capture", Some(&logs));
        assert!(diagnostic.contains("engine refused capture"), "{diagnostic}");
        assert!(diagnostic.contains("stdout-early-marker"), "{diagnostic}");
        assert!(diagnostic.contains("stderr-early-marker"), "{diagnostic}");
        assert!(diagnostic.contains("stdout-late-checkpoint-cause"), "{diagnostic}");
        assert!(diagnostic.contains("stderr-late-checkpoint-phase"), "{diagnostic}");
        assert!(diagnostic.contains("bytes]"), "{diagnostic}");
        assert!(diagnostic.len() < 2_048, "diagnostic was {} bytes", diagnostic.len());
    }
}

fn cleanup_timeout_diagnostic(timeout: Duration) -> String {
    format!("forced cleanup timed out after {} milliseconds", timeout.as_millis())
}

impl CaseExecution<'_> {
    async fn wait(&self, name: &str, timeout: Duration) -> Result<ExitStatus, Error> {
        let waiting = self.containers.wait(name);
        tokio::pin!(waiting);
        let deadline = Instant::now() + timeout;
        loop {
            tokio::select! {
                result = &mut waiting => return Ok(result?),
                () = tokio::time::sleep_until(deadline) => {
                    let logs = self.containers.logs(name).await?;
                    logs.bounded()?;
                    return Err(format!(
                        "guest exit wait timed out after {} milliseconds; stderr={}; stdout={}",
                        timeout.as_millis(),
                        logs.stderr.preview(),
                        logs.stdout.preview()
                    ).into());
                }
                () = tokio::time::sleep(Duration::from_millis(10)) => {
                    self.containers.logs(name).await?.bounded()?;
                }
            }
        }
    }
}

#[cfg(test)]
mod stderr_tests {
    use super::retention::HashedBoundedWriter;
    use super::{CLEANUP_TIMEOUT, FailureRetention, cleanup_timeout_diagnostic, materialize};
    use crate::{runtime::definition::App, suite::Target};
    use hl_container::{ExitStatus, Logs};
    use std::io::Write as _;

    fn missing_profile() -> Result<(), super::Error> {
        Err("retained C profile omitted the crossings/translations summary".into())
    }

    #[test]
    fn cleanup_timeout_names_the_stuck_lifecycle_stage() {
        assert_eq!(
            cleanup_timeout_diagnostic(CLEANUP_TIMEOUT),
            "forced cleanup timed out after 10000 milliseconds"
        );
    }

    #[test]
    fn exit_failure_precedes_missing_profile() {
        let error = super::super::outcome::validate(
            ExitStatus::Code(7),
            0,
            None,
            &Logs::default(),
            b"",
            &[],
            missing_profile(),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "exit Code(7), expected Code(0)");
    }

    #[test]
    fn stdout_failure_precedes_missing_profile() {
        let logs = Logs {
            stdout: b"wrong".to_vec(),
            stderr: Vec::new(),
        };
        let error =
            super::super::outcome::validate(ExitStatus::Code(0), 0, None, &logs, b"right", &[], missing_profile())
                .unwrap_err();
        assert!(error.to_string().starts_with("stdout differs:"), "{error}");
    }

    #[test]
    fn otherwise_valid_output_still_requires_profile() {
        let error = super::super::outcome::validate(
            ExitStatus::Code(0),
            0,
            None,
            &Logs::default(),
            b"",
            &[],
            missing_profile(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("crossings/translations"), "{error}");
    }

    #[tokio::test]
    async fn scratch_declaration_bypasses_the_document_image_and_yields_an_empty_root() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("probe.c"), "probe").unwrap();
        std::fs::create_dir(directory.path().join("golden")).unwrap();
        std::fs::write(directory.path().join("golden/probe.out"), []).unwrap();
        let definition = directory.path().join("test.yaml");
        std::fs::write(
            &definition,
            "targets: [arm64]\nimage: ':not-an-image'\nexecution: {}\nbuild:\n  compiler: { arm64: cc, amd64: cc }\n  flags: []\ncases:\n  - id: runtime/scratch-dispatch\n    status: active\n    compat: { class: compatibility }\n    rootfs: scratch\n    build: { source: probe.c, output: probe, flags: [] }\n    artifact: { destination: /probe }\n    run: []\n    expect: { exit: 0, stdout: golden/probe.out }\n",
        )
        .unwrap();
        let app = App::load(directory.path(), &definition).unwrap();

        let fixture = materialize(&app, &app.cases[0], Target::Arm64, super::Materialization::Overlay)
            .await
            .unwrap();
        assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(fixture.lower().unwrap()).unwrap().count(), 0);
        fixture.release().unwrap();
    }

    #[test]
    fn retained_overlay_writer_refuses_the_first_byte_past_its_bound() {
        let mut writer = HashedBoundedWriter::new(Vec::new(), 3);
        writer.write_all(b"abc").unwrap();
        let error = writer.write_all(b"d").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::FileTooLarge);
        let (bytes, digest) = writer.finish();
        assert_eq!(bytes, 3);
        assert_eq!(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn failed_overlay_archive_and_signal_manifest_are_correlated_before_release() {
        let platform = Target::Arm64.platform();
        let fixture = super::TestImage::materialize_scratch(&platform, super::Materialization::Overlay).unwrap();
        std::fs::write(fixture.path().join("layout-dependent.pyc"), b"failed-layout").unwrap();
        let artifact_dir = tempfile::tempdir().unwrap();
        let artifact = artifact_dir.path().join("python");
        std::fs::write(&artifact, b"exact-python-binary").unwrap();
        let output = tempfile::tempdir().unwrap();
        let retention = FailureRetention::new(output.path().to_owned(), "a".repeat(64));
        let status = ExitStatus::Fault {
            status: 11,
            detail: 0x1234,
            reason: hl_container::FaultCause::Memory,
        };
        let retained = retention
            .retain(
                &fixture,
                &artifact,
                "runtime/python/layout",
                Target::Arm64,
                7,
                Some(status),
            )
            .unwrap();

        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(retained.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["attempt"], 7);
        assert_eq!(manifest["status"]["kind"], "fault");
        assert_eq!(manifest["status"]["value"]["detail"], 0x1234);
        assert_eq!(
            manifest["artifact_sha256"],
            "509c1bf3e9e87736aaf9c75daf41a40532c0402ec32e4de571aff181a1bfae63"
        );
        let archive = std::fs::File::open(retained.join("upper.tar")).unwrap();
        let mut archive = tar::Archive::new(archive);
        let mut entries = archive.entries().unwrap();
        assert!(entries.any(|entry| entry.unwrap().path().unwrap().ends_with("layout-dependent.pyc")));
        fixture.release().unwrap();
        assert!(
            retained.join("upper.tar").is_file(),
            "release must not consume retained evidence"
        );
    }
}
