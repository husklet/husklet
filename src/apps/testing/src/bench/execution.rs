use super::{
    Error,
    definition::{Benchmark, BenchmarkCase, LifecyclePhase},
};
use crate::{runtime::image::TestImage, suite::Target};
use hl_container::{Config, ContainerSpec, Entry, ExitStatus, Isolation, Process, Sandbox, Session};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    sync::Arc,
    time::{Duration, Instant},
};

pub enum Result {
    Passed(Passed),
    Failed(Failed),
}

pub struct Passed {
    pub id: String,
    pub cold: u128,
    pub samples: Vec<u128>,
    pub phases: BTreeMap<String, Vec<u128>>,
    pub lifecycle: BTreeMap<String, Vec<u128>>,
    pub cold_lifecycle: BTreeMap<String, u128>,
    pub setup: BTreeMap<String, u128>,
    pub provenance: Provenance,
}

pub struct Failed {
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

pub struct Prepared {
    artifact: Option<std::path::PathBuf>,
    artifact_identity: Option<String>,
    image_identity: String,
    pub identity: String,
    setup: BTreeMap<String, u128>,
}

pub struct Measurement {
    pub cold: u128,
    pub samples: Vec<u128>,
    pub phases: BTreeMap<String, Vec<u128>>,
    pub lifecycle: BTreeMap<String, Vec<u128>>,
    pub cold_lifecycle: BTreeMap<String, u128>,
    pub setup: BTreeMap<String, u128>,
}

const CAPTURE_LIMIT: usize = 1024 * 1024;
const DIAGNOSTIC_CAPTURE: usize = 4096;
const DIAGNOSTIC_OUTPUT: usize = 16 * 1024;
const SETUP_ALLOWANCE: Duration = Duration::from_secs(120);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn prepare(benchmark: &Benchmark, case_index: usize, target: Target) -> std::result::Result<Prepared, Error> {
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
    let artifact_identity = artifact.as_deref().map(crate::record::FramedIdentity::of_file).transpose()?;
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
    let runner = std::env::current_exe()?;
    let runner_identity = crate::record::FramedIdentity::of_file(&runner)?;
    let definition = fs::read(benchmark.directory.join("test.yaml"))?;
    let source = artifact.as_ref().map_or_else(
        || Ok(benchmark.rootfs_executable.as_deref().unwrap_or_default().as_bytes().to_vec()),
        |_| fs::read(benchmark.source_path()),
    )?;
    let golden = fs::read(&case.stdout_contains)?;
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
    Ok(Prepared {
        artifact,
        artifact_identity,
        image_identity,
        identity,
        setup,
    })
}

pub async fn run(benchmark: Arc<Benchmark>, case_index: usize, target: Target, prepared: Prepared) -> Result {
    let case = &benchmark.cases[case_index];
    let identity = prepared.identity.clone();
    match execute(Arc::clone(&benchmark), case_index, target, prepared).await {
        Ok(measurement) => Result::Passed(Passed {
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
        Err(error) => Result::Failed(Failed {
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
    prepared: Prepared,
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
        && crate::record::FramedIdentity::of_file(artifact)? != *identity {
            image.release()?;
            return Err("benchmark artifact identity changed after resume admission".into());
        }
    let image_us = elapsed_us(image_started);
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
    let state = isolated_state()?;
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

fn elapsed_us(started: Instant) -> u128 {
    started.elapsed().as_micros()
}

fn isolated_state() -> std::result::Result<tempfile::TempDir, Error> {
    Ok(tempfile::tempdir()?)
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
    let expected_stdout = fs::read(&case.stdout_contains)?;
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
        .execution(benchmark.execution.container()?)
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
        bounded(&logs)?;
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
            .filter(|(phase, _)| self.benchmark_phases().contains(phase))
            .map(|(phase, time)| (phase.name().into(), time))
            .collect();
        Ok((elapsed, parse_phases(&logs.stdout)?, lifecycle))
    }

    fn benchmark_phases(&self) -> &[LifecyclePhase] {
        self.phases
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
    if captured > CAPTURE_LIMIT {
        Err(format!("captured output exceeded {CAPTURE_LIMIT} bytes").into())
    } else {
        Ok(captured)
    }
}

fn bounded(logs: &hl_container::Logs) -> std::result::Result<(), Error> {
    let captured = logs
        .stdout
        .len()
        .checked_add(logs.stderr.len())
        .ok_or("captured output size overflow")?;
    if captured > CAPTURE_LIMIT {
        Err(format!("captured output exceeded {CAPTURE_LIMIT} bytes").into())
    } else {
        Ok(())
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
        if name != "syscall" && phase.0 != checksum {
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
            Ok((name.to_owned(), time, checksum))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        Benchmark, CAPTURE_LIMIT, DIAGNOSTIC_OUTPUT, bounded, capture_size, isolated_state, output_excerpt,
        parse_phases, stdout_contains,
    };
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
            stdout: vec![0; CAPTURE_LIMIT - 1],
            stderr: vec![0],
        };
        assert!(bounded(&within).is_ok());
        let over = hl_container::Logs {
            stdout: vec![0; CAPTURE_LIMIT],
            stderr: vec![0],
        };
        assert!(bounded(&over).is_err());
    }

    #[test]
    fn incremental_capture_preserves_the_combined_limit() {
        let captured = capture_size(0, &entry(CAPTURE_LIMIT - 1)).unwrap();
        assert_eq!(capture_size(captured, &entry(1)).unwrap(), CAPTURE_LIMIT);
        assert!(capture_size(CAPTURE_LIMIT, &entry(1)).is_err());
        assert!(capture_size(usize::MAX, &entry(1)).is_err());
    }

    #[test]
    fn every_case_receives_independent_state() {
        let first = isolated_state().unwrap();
        let second = isolated_state().unwrap();
        assert_ne!(first.path(), second.path());
    }

    #[test]
    fn failure_diagnostic_does_not_repeat_the_full_capture() {
        let excerpt = output_excerpt(&vec![0xff; CAPTURE_LIMIT]);
        assert!(excerpt.len() <= DIAGNOSTIC_OUTPUT);
        assert!(excerpt.contains("truncated"));
    }
}
