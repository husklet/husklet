use super::{
    Error,
    definition::{Benchmark, BenchmarkCase, LifecyclePhase},
};
use crate::{
    runtime::image::TestImage,
    suite::{BoundedCapture as _, Capture, Target},
};
use hl_container::{Config, ContainerSpec, Entry, ExitStatus, Isolation, Process, Sandbox, Session};
use std::{
    collections::BTreeMap,
    error::Error as StdError,
    fmt, fs,
    os::unix::fs::PermissionsExt,
    sync::Arc,
    time::{Duration, Instant},
};

pub enum Result {
    Passed(Success),
    Failed(Failure),
}

pub struct Success {
    pub id: String,
    pub cold: u128,
    pub samples: Vec<u128>,
    pub phases: BTreeMap<String, Vec<u128>>,
    pub lifecycle: BTreeMap<String, Vec<u128>>,
    pub cold_lifecycle: BTreeMap<String, u128>,
    pub setup: BTreeMap<String, u128>,
    pub provenance: Provenance,
}

pub struct Failure {
    pub id: String,
    pub target: &'static str,
    pub reason: String,
}

pub struct Provenance {
    pub image: String,
    pub execution: String,
    pub target: &'static str,
    pub warmups: u32,
    pub identity: String,
}

pub struct Preparation {
    artifact: Option<std::path::PathBuf>,
    artifact_identity: Option<String>,
    image_identity: String,
    pub identity: String,
    setup: BTreeMap<String, u128>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProductBackend {
    ExplicitC,
    DefaultC,
}

impl ProductBackend {
    pub const fn name(self) -> &'static str {
        match self {
            Self::ExplicitC => "explicit-c",
            Self::DefaultC => "default-c",
        }
    }

    fn execution(self) -> hl_container::Execution {
        match self {
            Self::ExplicitC => hl_container::Execution::retained_c(),
            Self::DefaultC => hl_container::Execution::native(false),
        }
    }
}

pub struct ProductSample {
    pub round: u32,
    pub position: u8,
    pub backend: ProductBackend,
    pub setup_us: u128,
    pub execution_us: u128,
    pub teardown_us: u128,
    pub total_us: u128,
    pub output_identity: String,
}

pub struct ProductRun {
    pub setup: BTreeMap<String, u128>,
    pub samples: Vec<ProductSample>,
}

#[derive(Debug)]
struct ProductArmError {
    round: u32,
    position: usize,
    backend: ProductBackend,
    stage: &'static str,
    source: Error,
}

impl fmt::Display for ProductArmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "product A/B round {} position {} backend {} {}: {}",
            self.round,
            self.position,
            self.backend.name(),
            self.stage,
            self.source
        )
    }
}

impl StdError for ProductArmError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

fn product_arm_error(
    round: u32,
    position: usize,
    backend: ProductBackend,
    stage: &'static str,
    source: impl Into<Error>,
) -> Error {
    Box::new(ProductArmError {
        round,
        position,
        backend,
        stage,
        source: source.into(),
    })
}

pub struct Measurement {
    pub cold: u128,
    pub samples: Vec<u128>,
    pub phases: BTreeMap<String, Vec<u128>>,
    pub lifecycle: BTreeMap<String, Vec<u128>>,
    pub cold_lifecycle: BTreeMap<String, u128>,
    pub setup: BTreeMap<String, u128>,
}

const DIAGNOSTIC_CAPTURE: usize = 4096;
const DIAGNOSTIC_OUTPUT: usize = 16 * 1024;
const SETUP_ALLOWANCE: Duration = Duration::from_secs(120);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn run_product_ab(
    benchmark: Arc<Benchmark>,
    case_index: usize,
    target: Target,
    prepared: Preparation,
    rounds: u32,
) -> std::result::Result<ProductRun, Error> {
    let case = &benchmark.cases[case_index];
    let invocations = rounds.checked_mul(2).ok_or("product A/B invocation count overflow")?;
    let deadline = tokio::time::Instant::now()
        + Duration::from_secs(case.timeout)
            .checked_mul(invocations)
            .and_then(|duration| duration.checked_add(SETUP_ALLOWANCE))
            .ok_or("product A/B deadline overflow")?;
    let image_started = Instant::now();
    let image = tokio::time::timeout_at(deadline, TestImage::materialize(&benchmark.image, &target.platform()))
        .await
        .map_err(|_| "product A/B image materialization exceeded its deadline")??;
    if image.identity() != prepared.image_identity {
        image.release()?;
        return Err("benchmark image identity changed after product A/B preparation".into());
    }
    if let (Some(artifact), Some(identity)) = (&prepared.artifact, &prepared.artifact_identity)
        && crate::record::FramedIdentity::of_file(artifact)? != *identity
    {
        image.release()?;
        return Err("benchmark artifact identity changed after product A/B preparation".into());
    }
    let mut setup = prepared.setup;
    setup.insert("image_materialize".into(), elapsed_us(image_started));
    let outcome = run_product_ab_with_image(
        &benchmark,
        case,
        target,
        image.path(),
        prepared.artifact.as_deref(),
        rounds,
        deadline,
        &mut setup,
    )
    .await;
    let release_started = Instant::now();
    let release = tokio::time::timeout(
        CLEANUP_TIMEOUT,
        tokio::task::spawn_blocking(move || image.release().map_err(|error| error.to_string())),
    )
    .await
    .map_err(|_| "product A/B image cleanup timed out")??;
    release?;
    setup.insert("image_release".into(), elapsed_us(release_started));
    Ok(ProductRun {
        setup,
        samples: outcome?,
    })
}

async fn run_product_ab_with_image(
    benchmark: &Benchmark,
    case: &BenchmarkCase,
    target: Target,
    image: &std::path::Path,
    artifact: Option<&std::path::Path>,
    rounds: u32,
    deadline: tokio::time::Instant,
    setup: &mut BTreeMap<String, u128>,
) -> std::result::Result<Vec<ProductSample>, Error> {
    let started = Instant::now();
    let state = tempfile::tempdir()?;
    let containers = tokio::time::timeout_at(
        deadline,
        hl_container::Containers::builder(Config::new(state.path())).build(),
    )
    .await
    .map_err(|_| "product A/B container service setup exceeded its deadline")??;
    setup.insert("container_service".into(), elapsed_us(started));
    let program = benchmark
        .rootfs_executable
        .clone()
        .unwrap_or_else(|| format!("/opt/husklet/bench-{}", case.id));
    let started = Instant::now();
    if let Some(artifact) = artifact {
        stage(artifact, image, &program)?;
    }
    setup.insert("guest_stage".into(), elapsed_us(started));
    let expected_stdout = tokio::fs::read(&case.stdout_contains).await?;
    let mut expected_identity = None;
    let mut samples = Vec::with_capacity((rounds * 2) as usize);
    for round in 0..rounds {
        for (position, backend) in product_order(round).into_iter().enumerate() {
            let name_repetition = round * 2 + u32::try_from(position)?;
            let invocation = Invocation::new_with_execution(
                &containers,
                benchmark,
                case,
                target,
                image,
                &program,
                name_repetition,
                backend.execution(),
            )
            .map_err(|error| product_arm_error(round, position, backend, "specification", error))?;
            let outcome = tokio::time::timeout_at(deadline, invocation.execute_captured(&expected_stdout))
                .await
                .map_err(|_| {
                    product_arm_error(
                        round,
                        position,
                        backend,
                        "execution",
                        "exceeded the product A/B deadline",
                    )
                })?
                .map_err(|error| product_arm_error(round, position, backend, "execution", error))?;
            let teardown_started = Instant::now();
            tokio::time::timeout(CLEANUP_TIMEOUT, containers.remove_force(&invocation.name))
                .await
                .map_err(|_| product_arm_error(round, position, backend, "cleanup", "container cleanup timed out"))?
                .map_err(|error| product_arm_error(round, position, backend, "cleanup", error))?;
            let remove_us = elapsed_us(teardown_started);
            let identity =
                crate::record::FramedIdentity::over(&[outcome.stdout.as_slice(), outcome.stderr.as_slice()])?;
            if let Some(expected) = &expected_identity {
                if expected != &identity {
                    return Err(format!("product A/B output changed at round {round} position {position}").into());
                }
            } else {
                expected_identity = Some(identity.clone());
            }
            let setup_us = lifecycle_sum(&outcome.lifecycle, &["create", "attach", "start"]);
            let execution_us = lifecycle_sum(&outcome.lifecycle, &["wait_and_drain"]);
            let teardown_us = lifecycle_sum(&outcome.lifecycle, &["output_read"]).saturating_add(remove_us);
            samples.push(ProductSample {
                round,
                position: u8::try_from(position)?,
                backend,
                setup_us,
                execution_us,
                teardown_us,
                total_us: setup_us.saturating_add(execution_us).saturating_add(teardown_us),
                output_identity: identity,
            });
        }
    }
    Ok(samples)
}

fn lifecycle_sum(values: &[(String, u128)], names: &[&str]) -> u128 {
    values
        .iter()
        .filter(|(name, _)| names.contains(&name.as_str()))
        .map(|(_, value)| *value)
        .sum()
}

fn product_order(round: u32) -> [ProductBackend; 2] {
    if round % 2 == 0 {
        [ProductBackend::ExplicitC, ProductBackend::DefaultC]
    } else {
        [ProductBackend::DefaultC, ProductBackend::ExplicitC]
    }
}

pub async fn prepare(
    benchmark: &Benchmark,
    case_index: usize,
    target: Target,
) -> std::result::Result<Preparation, Error> {
    let case = &benchmark.cases[case_index];
    let mut setup = BTreeMap::new();
    let started = Instant::now();
    let artifact = if benchmark.rootfs_executable.is_some() {
        None
    } else {
        Some(benchmark.build(case, target).await?)
    };
    setup.insert("provenance_build".into(), elapsed_us(started));
    let started = Instant::now();
    let artifact_identity = artifact
        .as_deref()
        .map(crate::record::FramedIdentity::of_file)
        .transpose()?;
    setup.insert("provenance_artifact_identity".into(), elapsed_us(started));
    let compiler = benchmark
        .rootfs_executable
        .as_deref()
        .map_or_else(|| benchmark.compiler_name(target), |_| "rootfs");
    let started = Instant::now();
    let compiler_version = if artifact.is_some() {
        let compiler = benchmark.compiler_name(target);
        let output = tokio::process::Command::new(compiler)
            .arg("--version")
            .kill_on_drop(true)
            .output()
            .await?;
        if !output.status.success() || output.stdout.len() > 64 * 1024 {
            return Err(format!("{compiler} did not provide bounded compiler provenance").into());
        }
        output.stdout
    } else {
        b"rootfs-workload-v1".to_vec()
    };
    setup.insert("provenance_compiler_identity".into(), elapsed_us(started));
    let started = Instant::now();
    let image_identity = TestImage::resolve_identity(&benchmark.image, &target.platform()).await?;
    setup.insert("provenance_image_identity".into(), elapsed_us(started));
    let started = Instant::now();
    let runner = crate::runtime::profile::runner()?;
    let runner_identity = crate::record::FramedIdentity::of_file(&runner)?;
    let definition = tokio::fs::read(benchmark.directory.join("test.yaml")).await?;
    let source = match artifact.as_ref() {
        Some(_) => tokio::fs::read(benchmark.source_path()).await?,
        None => benchmark
            .rootfs_executable
            .as_deref()
            .unwrap_or_default()
            .as_bytes()
            .to_vec(),
    };
    let golden = tokio::fs::read(&case.stdout_contains).await?;
    let execution = format!("{:?}", benchmark.execution);
    let identity = crate::record::FramedIdentity::over(&[
        b"husklet-benchmark-provenance-v1".as_slice(),
        target.name().as_bytes(),
        execution.as_bytes(),
        compiler.as_bytes(),
        compiler_version.as_slice(),
        artifact_identity.as_deref().unwrap_or("rootfs").as_bytes(),
        image_identity.as_bytes(),
        runner_identity.as_bytes(),
        definition.as_slice(),
        source.as_slice(),
        golden.as_slice(),
    ])?;
    setup.insert("provenance_hash".into(), elapsed_us(started));
    Ok(Preparation {
        artifact,
        artifact_identity,
        image_identity,
        identity,
        setup,
    })
}

pub async fn run(benchmark: Arc<Benchmark>, case_index: usize, target: Target, prepared: Preparation) -> Result {
    let case = &benchmark.cases[case_index];
    let identity = prepared.identity.clone();
    match execute(Arc::clone(&benchmark), case_index, target, prepared).await {
        Ok(measurement) => Result::Passed(Success {
            id: format!("{}/{}", benchmark.name, case.id),
            cold: measurement.cold,
            samples: measurement.samples,
            phases: measurement.phases,
            lifecycle: measurement.lifecycle,
            cold_lifecycle: measurement.cold_lifecycle,
            setup: measurement.setup,
            provenance: Provenance {
                image: benchmark.image.clone(),
                execution: format!("{:?}", benchmark.execution),
                target: target.name(),
                warmups: case.warmups,
                identity,
            },
        }),
        Err(error) => Result::Failed(Failure {
            id: format!("{}/{}", benchmark.name, case.id),
            target: target.name(),
            reason: error.to_string(),
        }),
    }
}

async fn execute(
    benchmark: Arc<Benchmark>,
    case_index: usize,
    target: Target,
    prepared: Preparation,
) -> std::result::Result<Measurement, Error> {
    let case = &benchmark.cases[case_index];
    let deadline = tokio::time::Instant::now() + row_timeout(case)?;
    let image_started = Instant::now();
    let image = tokio::time::timeout_at(deadline, TestImage::materialize(&benchmark.image, &target.platform()))
        .await
        .map_err(|_| "benchmark image materialization exceeded the total row deadline")??;
    if image.identity() != prepared.image_identity {
        image.release()?;
        return Err("benchmark image identity changed after resume admission".into());
    }
    if let (Some(artifact), Some(identity)) = (&prepared.artifact, &prepared.artifact_identity)
        && crate::record::FramedIdentity::of_file(artifact)? != *identity
    {
        image.release()?;
        return Err("benchmark artifact identity changed after resume admission".into());
    }
    let image_us = elapsed_us(image_started);
    // Attribution probe: reading the fresh rootfs sequentially isolates first-touch demand paging
    // from the cold measurement. Off unless HL_BENCH_ROOTFS_PREFETCH is set.
    let prefetch_started = Instant::now();
    if std::env::var_os("HL_BENCH_ROOTFS_PREFETCH").is_some() {
        let root = image.path().to_path_buf();
        tokio::task::spawn_blocking(move || prefetch_tree(&root)).await?;
    }
    let prefetch_us = elapsed_us(prefetch_started);
    let outcome = execute_with_image(
        Arc::clone(&benchmark),
        case_index,
        target,
        image.path(),
        prepared.artifact.as_deref(),
        deadline,
    )
    .await
    .map_err(|error| error.to_string());
    let cleanup_started = Instant::now();
    let release = tokio::time::timeout(
        CLEANUP_TIMEOUT,
        tokio::task::spawn_blocking(move || image.release().map_err(|error| error.to_string())),
    )
    .await
    .map_err(|_| "benchmark image cleanup timed out")??;
    match outcome {
        Ok(mut result) => {
            release?;
            result.setup.extend(prepared.setup);
            result.setup.insert("execution_image_materialize".into(), image_us);
            result.setup.insert("execution_rootfs_prefetch".into(), prefetch_us);
            result
                .setup
                .insert("execution_image_release".into(), elapsed_us(cleanup_started));
            Ok(result)
        }
        Err(error) => {
            let _ = release;
            Err(error.into())
        }
    }
}

async fn execute_with_image(
    benchmark: Arc<Benchmark>,
    case_index: usize,
    target: Target,
    image: &std::path::Path,
    artifact: Option<&std::path::Path>,
    deadline: tokio::time::Instant,
) -> std::result::Result<Measurement, Error> {
    let service_started = Instant::now();
    let state = tempfile::tempdir()?;
    let containers = tokio::time::timeout_at(
        deadline,
        hl_container::Containers::builder(Config::new(state.path())).build(),
    )
    .await
    .map_err(|_| "benchmark container setup exceeded the total row deadline")??;
    let service_us = elapsed_us(service_started);
    let case = &benchmark.cases[case_index];
    let guest_program = benchmark
        .rootfs_executable
        .clone()
        .unwrap_or_else(|| format!("/opt/husklet/bench-{}", case.id));
    let stage_started = Instant::now();
    if let Some(artifact) = artifact {
        let staging_artifact = artifact.to_path_buf();
        let staging_image = image.to_path_buf();
        let staging_program = guest_program.clone();
        tokio::time::timeout_at(
            deadline,
            tokio::task::spawn_blocking(move || {
                stage(&staging_artifact, &staging_image, &staging_program).map_err(|error| error.to_string())
            }),
        )
        .await
        .map_err(|_| "benchmark staging exceeded the total row deadline")???;
    }
    let stage_us = elapsed_us(stage_started);
    let mut measurement = run_case(&containers, &benchmark, case, target, image, &guest_program, deadline).await?;
    measurement.setup.insert("container_service".into(), service_us);
    measurement.setup.insert("guest_stage".into(), stage_us);
    Ok(measurement)
}

/// Reads every regular file in the tree so the page cache is warm before the first invocation.
fn prefetch_tree(root: &std::path::Path) {
    let mut pending = vec![root.to_path_buf()];
    let mut buffer = vec![0_u8; 1 << 20];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                read_fully(&entry.path(), &mut buffer);
            }
        }
    }
}

fn read_fully(path: &std::path::Path, buffer: &mut [u8]) {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return;
    };
    while let Ok(read) = file.read(buffer) {
        if read == 0 {
            break;
        }
    }
}

fn elapsed_us(started: Instant) -> u128 {
    started.elapsed().as_micros()
}

fn stage(artifact: &std::path::Path, image: &std::path::Path, program: &str) -> std::result::Result<(), Error> {
    let destination = image.join(program.trim_start_matches('/'));
    fs::create_dir_all(destination.parent().ok_or("benchmark destination has no parent")?)?;
    fs::copy(artifact, &destination)?;
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn row_timeout(case: &BenchmarkCase) -> std::result::Result<Duration, Error> {
    let invocations = 1_u32
        .checked_add(case.warmups)
        .and_then(|value| value.checked_add(case.samples))
        .ok_or("benchmark invocation count overflow")?;
    Duration::from_secs(case.timeout)
        .checked_mul(invocations)
        .and_then(|value| value.checked_add(SETUP_ALLOWANCE))
        .ok_or_else(|| "benchmark row timeout overflow".into())
}

async fn run_case(
    containers: &hl_container::Containers,
    benchmark: &Benchmark,
    case: &BenchmarkCase,
    target: Target,
    image: &std::path::Path,
    program: &str,
    deadline: tokio::time::Instant,
) -> std::result::Result<Measurement, Error> {
    let expected_stdout = tokio::fs::read(&case.stdout_contains).await?;
    let total = 1_u32
        .checked_add(case.warmups)
        .and_then(|value| value.checked_add(case.samples))
        .ok_or("benchmark invocation count overflow")?;
    let mut measurements = Measurements::new(case.samples);
    for repetition in 0..total {
        let invocation = Invocation::new(containers, benchmark, case, target, image, program, repetition)?;
        let outcome = tokio::time::timeout_at(deadline, invocation.execute(&expected_stdout))
            .await
            .map_err(|_| "benchmark exceeded the total row deadline".to_owned())
            .and_then(|result| result.map_err(|error| error.to_string()));
        let cleanup = tokio::time::timeout(CLEANUP_TIMEOUT, containers.remove_force(&invocation.name))
            .await
            .map_err(|_| "benchmark container cleanup timed out".to_owned())
            .and_then(|result| result.map_err(|error| error.to_string()));
        match outcome {
            Ok((elapsed, invocation_phases, lifecycle)) => {
                cleanup.map_err(|error| -> Error { error.into() })?;
                measurements.record(repetition, case.warmups, elapsed, invocation_phases, lifecycle)?;
            }
            Err(error) => {
                let _ = cleanup;
                return Err(error.into());
            }
        }
    }
    measurements.finish(case.samples)
}

struct Invocation<'a> {
    containers: &'a hl_container::Containers,
    case: &'a BenchmarkCase,
    phases: &'a [LifecyclePhase],
    name: String,
    spec: ContainerSpec,
}

impl<'a> Invocation<'a> {
    fn new(
        containers: &'a hl_container::Containers,
        benchmark: &'a Benchmark,
        case: &'a BenchmarkCase,
        target: Target,
        image: &std::path::Path,
        program: &str,
        repetition: u32,
    ) -> std::result::Result<Self, Error> {
        Self::new_with_execution(
            containers,
            benchmark,
            case,
            target,
            image,
            program,
            repetition,
            benchmark.execution.container()?,
        )
    }

    fn new_with_execution(
        containers: &'a hl_container::Containers,
        benchmark: &'a Benchmark,
        case: &'a BenchmarkCase,
        target: Target,
        image: &std::path::Path,
        program: &str,
        repetition: u32,
        execution: hl_container::Execution,
    ) -> std::result::Result<Self, Error> {
        let name = format!(
            "testing-bench-{}-{}-{}-{repetition}",
            benchmark.name,
            case.id,
            target.name()
        );
        let spec = ContainerSpec::from_directory(
            image,
            Process::new(program).args(case.arguments.iter().map(String::as_str)),
        )
        .name(&name)
        .guest(target.guest())
        .execution(execution)
        .isolation(Isolation {
            sandbox: Sandbox::Disabled,
            ..Isolation::default()
        });
        Ok(Self {
            containers,
            case,
            phases: &benchmark.phases,
            name,
            spec,
        })
    }

    async fn execute(
        &self,
        expected_stdout: &[u8],
    ) -> std::result::Result<(u128, Vec<(String, u128, u64)>, Vec<(String, u128)>), Error> {
        let mut outcome = self.execute_captured(expected_stdout).await?;
        outcome
            .lifecycle
            .retain(|(name, _)| self.benchmark_phases().iter().any(|phase| phase.name() == name));
        Ok((outcome.elapsed_ms, outcome.phases, outcome.lifecycle))
    }

    async fn execute_captured(&self, expected_stdout: &[u8]) -> std::result::Result<InvocationOutcome, Error> {
        let started = Instant::now();
        let phase_started = Instant::now();
        self.containers.create(self.spec.clone()).await?;
        let create_us = elapsed_us(phase_started);
        let phase_started = Instant::now();
        let mut output = self.containers.attach(&self.name).await?;
        let attach_us = elapsed_us(phase_started);
        let phase_started = Instant::now();
        self.containers.start(&self.name).await?;
        let start_us = elapsed_us(phase_started);
        let phase_started = Instant::now();
        let status = self.wait(&mut output).await?;
        let execution_us = elapsed_us(phase_started);
        let elapsed = started.elapsed().as_millis();
        let phase_started = Instant::now();
        let logs = self.containers.logs(&self.name).await?;
        let logs_us = elapsed_us(phase_started);
        logs.bounded()?;
        if status != ExitStatus::Code(self.case.exit) {
            return Err(format!("exit {status:?}, expected {}", self.case.exit).into());
        }
        if expected_stdout.is_empty() && !logs.stdout.is_empty() {
            return Err(format!("expected empty stdout; stdout={}", output_excerpt(&logs.stdout)).into());
        }
        if !stdout_contains(&logs.stdout, expected_stdout) {
            return Err(format!(
                "stdout missing marker from {}; stdout={}",
                self.case.stdout_contains.display(),
                output_excerpt(&logs.stdout)
            )
            .into());
        }
        if !logs.stderr.is_empty() {
            return Err(format!("unexpected stderr: {}", output_excerpt(&logs.stderr)).into());
        }
        let values = [create_us, attach_us, start_us, execution_us, logs_us];
        let all = [
            LifecyclePhase::Create,
            LifecyclePhase::Attach,
            LifecyclePhase::Start,
            LifecyclePhase::WaitAndDrain,
            LifecyclePhase::OutputRead,
        ];
        let lifecycle = all
            .into_iter()
            .zip(values)
            .map(|(phase, time)| (phase.name().into(), time))
            .collect();
        Ok(InvocationOutcome {
            elapsed_ms: elapsed,
            phases: parse_phases(&logs.stdout)?,
            lifecycle,
            stdout: logs.stdout,
            stderr: logs.stderr,
        })
    }

    fn benchmark_phases(&self) -> &[LifecyclePhase] {
        self.phases
    }
}

struct InvocationOutcome {
    elapsed_ms: u128,
    phases: Vec<(String, u128, u64)>,
    lifecycle: Vec<(String, u128)>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[cfg(test)]
mod product_tests {
    use super::{ProductBackend, product_arm_error, product_order};

    #[test]
    fn product_pair_order_balances_first_position() {
        assert_eq!(product_order(0), [ProductBackend::ExplicitC, ProductBackend::DefaultC]);
        assert_eq!(product_order(1), [ProductBackend::DefaultC, ProductBackend::ExplicitC]);
        assert_eq!(product_order(2), [ProductBackend::ExplicitC, ProductBackend::DefaultC]);
        assert_eq!(product_order(3), [ProductBackend::DefaultC, ProductBackend::ExplicitC]);
    }

    #[test]
    fn product_arm_failure_names_context_and_preserves_source() {
        let error = product_arm_error(
            3,
            1,
            ProductBackend::DefaultC,
            "execution",
            std::io::Error::other("Construction(Start)"),
        );
        assert_eq!(
            error.to_string(),
            "product A/B round 3 position 1 backend default-c execution: Construction(Start)"
        );
        assert_eq!(error.source().unwrap().to_string(), "Construction(Start)");
    }
}

fn stdout_contains(stdout: &[u8], marker: &[u8]) -> bool {
    marker.is_empty() || stdout.windows(marker.len()).any(|window| window == marker)
}

fn output_excerpt(bytes: &[u8]) -> String {
    let shown = bytes.len().min(DIAGNOSTIC_CAPTURE);
    let suffix = bytes
        .len()
        .checked_sub(shown)
        .filter(|omitted| *omitted > 0)
        .map_or_else(String::new, |omitted| format!(" ... [{omitted} bytes omitted]"));
    let mut output = format!("{:?}{suffix}", &bytes[..shown]);
    if output.len() > DIAGNOSTIC_OUTPUT {
        const TRUNCATED: &str = " ... [excerpt truncated]";
        output.truncate(DIAGNOSTIC_OUTPUT - TRUNCATED.len());
        output.push_str(TRUNCATED);
    }
    output
}

impl Invocation<'_> {
    async fn wait(&self, output: &mut Session) -> std::result::Result<ExitStatus, Error> {
        let timeout = Duration::from_secs(self.case.timeout);
        let deadline = tokio::time::Instant::now() + timeout;
        let waiting = self.containers.wait(&self.name);
        tokio::pin!(waiting);
        let mut captured = 0;
        loop {
            tokio::select! {
                entry = output.next() => match entry? {
                    Some(entry) => captured = capture_size(captured, &entry)?,
                    None => return tokio::time::timeout_at(deadline, waiting)
                        .await
                        .map_err(|_| timeout_error(self.case.timeout))?
                        .map_err(Into::into),
                },
                status = &mut waiting => {
                    let status = status?;
                    capture_until(output, captured, deadline, self.case.timeout).await?;
                    return Ok(status);
                }
                () = tokio::time::sleep_until(deadline) => return Err(timeout_error(self.case.timeout)),
            }
        }
    }
}

async fn capture_until(
    output: &mut Session,
    mut captured: usize,
    deadline: tokio::time::Instant,
    timeout: u64,
) -> std::result::Result<(), Error> {
    loop {
        let entry = tokio::time::timeout_at(deadline, output.next())
            .await
            .map_err(|_| timeout_error(timeout))??;
        let Some(entry) = entry else { return Ok(()) };
        captured = capture_size(captured, &entry)?;
    }
}

fn timeout_error(seconds: u64) -> Error {
    format!("timed out after {seconds} seconds").into()
}

fn capture_size(captured: usize, entry: &Entry) -> std::result::Result<usize, Error> {
    let captured = captured
        .checked_add(entry.bytes.len())
        .ok_or("captured output size overflow")?;
    if captured > Capture::LIMIT {
        Err(format!("captured output exceeded {} bytes", Capture::LIMIT).into())
    } else {
        Ok(captured)
    }
}

struct Measurements {
    cold: Option<u128>,
    samples: Vec<u128>,
    phases: BTreeMap<String, (u64, Vec<u128>)>,
    lifecycle: BTreeMap<String, Vec<u128>>,
    cold_lifecycle: BTreeMap<String, u128>,
}

impl Measurements {
    fn new(samples: u32) -> Self {
        Self {
            cold: None,
            samples: Vec::with_capacity(samples as usize),
            phases: BTreeMap::new(),
            lifecycle: BTreeMap::new(),
            cold_lifecycle: BTreeMap::new(),
        }
    }

    fn record(
        &mut self,
        repetition: u32,
        warmups: u32,
        elapsed: u128,
        phases: Vec<(String, u128, u64)>,
        lifecycle: Vec<(String, u128)>,
    ) -> std::result::Result<(), Error> {
        if repetition == 0 {
            self.cold = Some(elapsed);
            self.cold_lifecycle.extend(lifecycle);
            return Ok(());
        }
        if repetition <= warmups {
            return Ok(());
        }
        self.samples.push(elapsed);
        for (name, time, checksum) in phases {
            self.record_phase(&name, time, checksum)?;
        }
        for (name, time) in lifecycle {
            self.lifecycle.entry(name).or_default().push(time);
        }
        Ok(())
    }

    fn record_phase(&mut self, name: &str, time: u128, checksum: u64) -> std::result::Result<(), Error> {
        let phase = self
            .phases
            .entry(name.to_owned())
            .or_insert_with(|| (checksum, Vec::new()));
        if phase.0 != checksum {
            return Err(format!("PHASE {name} checksum changed across samples").into());
        }
        phase.1.push(time);
        Ok(())
    }

    fn finish(self, samples: u32) -> std::result::Result<Measurement, Error> {
        if self.phases.values().any(|(_, times)| times.len() != samples as usize) {
            return Err("PHASE set changed across samples".into());
        }
        if self.lifecycle.values().any(|times| times.len() != samples as usize) {
            return Err("LIFECYCLE set changed across samples".into());
        }
        Ok(Measurement {
            cold: self.cold.ok_or("cold benchmark sample was not run")?,
            samples: self.samples,
            phases: self
                .phases
                .into_iter()
                .map(|(name, (_, times))| (name, times))
                .collect(),
            lifecycle: self.lifecycle,
            cold_lifecycle: self.cold_lifecycle,
            setup: BTreeMap::new(),
        })
    }
}

fn parse_phases(stdout: &[u8]) -> std::result::Result<Vec<(String, u128, u64)>, Error> {
    let text = std::str::from_utf8(stdout).map_err(|_| "benchmark stdout is not UTF-8")?;
    text.lines()
        .filter(|line| line.starts_with("PHASE "))
        .map(|line| {
            let mut fields = line.split_whitespace();
            let _protocol = fields.next();
            let name = fields.next().ok_or_else(|| format!("invalid PHASE row {line:?}"))?;
            let time = fields
                .next()
                .and_then(|field| field.strip_prefix("us="))
                .ok_or_else(|| format!("invalid PHASE time {line:?}"))?
                .parse::<u128>()?;
            let checksum = fields
                .next()
                .and_then(|field| field.strip_prefix("ok="))
                .ok_or_else(|| format!("invalid PHASE checksum {line:?}"))?
                .parse::<u64>()?;
            // Every phase counts the work it completed, so zero is a silent no-op, not a pass.
            if checksum == 0 {
                return Err(format!("PHASE {name} completed no work (ok=0)").into());
            }
            crate::benchmark::timebase_verdict(name, u64::try_from(time).unwrap_or(u64::MAX), checksum)?;
            Ok((name.to_owned(), time, checksum))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Benchmark, Capture, DIAGNOSTIC_OUTPUT, capture_size, output_excerpt, parse_phases, stdout_contains};
    use crate::suite::BoundedCapture as _;
    use hl_container::{Entry, Stream};

    fn entry(bytes: usize) -> Entry {
        Entry {
            sequence: 1,
            timestamp_ms: 1,
            stream: Stream::Stdout,
            bytes: vec![0; bytes],
        }
    }

    #[test]
    fn retained_phase_protocol_is_accepted() {
        let phases = parse_phases(b"noise\nPHASE compute us=42 ok=7\n").unwrap();
        assert_eq!(phases, vec![("compute".to_owned(), 42, 7)]);
        assert!(parse_phases(b"PHASE compute ms=42 ok=7\n").is_err());
    }

    /// A forged timebase leaves every work-counting checksum identical, so only the
    /// timebase row can refuse it. Measured on the host: declaring cntfrq=1e9 against a
    /// 24MHz counter reported ok=21 and us=2615 for a 100ms sleep, with all seventeen
    /// other checksums byte-identical to the correct binary's.
    #[test]
    fn a_forged_timebase_is_refused_although_every_other_checksum_matches() {
        let honest = b"PHASE timebase us=101848 ok=1\nPHASE compute us=117 ok=441094035400083178\n";
        assert_eq!(parse_phases(honest).unwrap().len(), 2);
        let forged = b"PHASE timebase us=2615 ok=21\nPHASE compute us=2 ok=441094035400083178\n";
        let error = parse_phases(forged).unwrap_err().to_string();
        assert!(error.contains("divergent guest timebase"), "{error}");
        // A guest whose clocks are wrong together still agrees with itself, so only the
        // duration of its own sleep can refuse it.
        let uniform = b"PHASE timebase us=2440 ok=1\n";
        assert!(parse_phases(uniform).unwrap_err().to_string().contains("100ms sleep"));
    }

    #[test]
    fn zero_work_count_is_rejected() {
        assert!(parse_phases(b"PHASE file us=100 ok=0\n").is_err());
    }

    #[test]
    fn provenance_is_typed_and_changes_with_every_identity_field() {
        let baseline = [
            b"provider".as_slice(),
            b"arm64",
            b"artifact",
            b"image",
            b"runner",
            b"definition",
        ];
        let expected = crate::record::FramedIdentity::over(&baseline).unwrap();
        for index in 0..baseline.len() {
            let mut changed = baseline;
            changed[index] = b"changed";
            assert_ne!(
                crate::record::FramedIdentity::over(&changed).unwrap(),
                expected,
                "field {index} did not bind provenance"
            );
        }
        assert_ne!(
            crate::record::FramedIdentity::over(&[b"ab", b"c"]).unwrap(),
            crate::record::FramedIdentity::over(&[b"a", b"bc"]).unwrap(),
        );
    }

    #[test]
    fn combined_definition_accepts_named_phase_rows() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/bench/combined");
        let benchmark = Benchmark::load(&directory, &directory.join("test.yaml")).unwrap();
        let marker = std::fs::read(&benchmark.cases[0].stdout_contains).unwrap();
        let phase = b"PHASE compute us=42 ok=7\n";

        assert!(stdout_contains(phase, &marker));
        assert!(stdout_contains(&[b"noise\n".as_slice(), phase].concat(), &marker));
    }

    #[test]
    fn file_io_definition_exposes_all_typed_phases() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/bench/file_io");
        let benchmark = Benchmark::load(&directory, &directory.join("test.yaml")).unwrap();
        assert_eq!(benchmark.cases.len(), 3);
        let phases =
            parse_phases(b"PHASE scalar_file us=11 ok=1\nPHASE vector_file us=12 ok=2\nPHASE mapped_file us=13 ok=3\n")
                .unwrap();
        assert_eq!(phases.len(), 3);
    }

    #[test]
    fn combined_capture_is_bounded() {
        let within = hl_container::Logs {
            stdout: vec![0; Capture::LIMIT - 1],
            stderr: vec![0],
        };
        assert!(within.bounded().is_ok());
        let over = hl_container::Logs {
            stdout: vec![0; Capture::LIMIT],
            stderr: vec![0],
        };
        assert!(over.bounded().is_err());
    }

    #[test]
    fn incremental_capture_preserves_the_combined_limit() {
        let captured = capture_size(0, &entry(Capture::LIMIT - 1)).unwrap();
        assert_eq!(capture_size(captured, &entry(1)).unwrap(), Capture::LIMIT);
        assert!(capture_size(Capture::LIMIT, &entry(1)).is_err());
        assert!(capture_size(usize::MAX, &entry(1)).is_err());
    }

    #[test]
    fn failure_diagnostic_does_not_repeat_the_full_capture() {
        let excerpt = output_excerpt(&vec![0xff; Capture::LIMIT]);
        assert!(excerpt.len() <= DIAGNOSTIC_OUTPUT);
        assert!(excerpt.contains("truncated"));
    }
}
