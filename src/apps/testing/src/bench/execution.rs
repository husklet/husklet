use super::{
    Error,
    definition::{Benchmark, BenchmarkCase},
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
}

type MeasurementResult = (u128, Vec<u128>, BTreeMap<String, Vec<u128>>);

const CAPTURE_LIMIT: usize = 1024 * 1024;
const DIAGNOSTIC_CAPTURE: usize = 4096;
const DIAGNOSTIC_OUTPUT: usize = 16 * 1024;
const SETUP_ALLOWANCE: Duration = Duration::from_secs(120);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn run(benchmark: Arc<Benchmark>, case_index: usize, target: Target) -> Result {
    let case = &benchmark.cases[case_index];
    match execute(Arc::clone(&benchmark), case_index, target).await {
        Ok((cold, samples, phases)) => Result::Passed(Passed {
            id: format!("{}/{}", benchmark.name, case.id),
            cold,
            samples,
            phases,
            provenance: Provenance {
                image: benchmark.image.clone(),
                execution: format!("{:?}", benchmark.execution),
                target: target.name(),
                warmups: case.warmups,
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
) -> std::result::Result<MeasurementResult, Error> {
    let case = &benchmark.cases[case_index];
    let deadline = tokio::time::Instant::now() + row_timeout(case)?;
    let image = tokio::time::timeout_at(deadline, TestImage::materialize(&benchmark.image, &target.platform()))
        .await
        .map_err(|_| "benchmark image materialization exceeded the total row deadline")??;
    let outcome = execute_with_image(Arc::clone(&benchmark), case_index, target, image.path(), deadline)
        .await
        .map_err(|error| error.to_string());
    let release = tokio::time::timeout(
        CLEANUP_TIMEOUT,
        tokio::task::spawn_blocking(move || image.release().map_err(|error| error.to_string())),
    )
    .await
    .map_err(|_| "benchmark image cleanup timed out")??;
    match outcome {
        Ok(result) => {
            release?;
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
    deadline: tokio::time::Instant,
) -> std::result::Result<MeasurementResult, Error> {
    let state = isolated_state()?;
    let containers = tokio::time::timeout_at(
        deadline,
        hl_container::Containers::builder(Config::new(state.path())).build(),
    )
    .await
    .map_err(|_| "benchmark container setup exceeded the total row deadline")??;
    let case = &benchmark.cases[case_index];
    let artifact = tokio::time::timeout_at(deadline, benchmark.build(case, target))
        .await
        .map_err(|_| "benchmark build exceeded the total row deadline")??;
    let guest_program = format!("/opt/husklet/bench-{}", case.id);
    let staging_image = image.to_path_buf();
    let staging_program = guest_program.clone();
    tokio::time::timeout_at(
        deadline,
        tokio::task::spawn_blocking(move || {
            stage(&artifact, &staging_image, &staging_program).map_err(|error| error.to_string())
        }),
    )
    .await
    .map_err(|_| "benchmark staging exceeded the total row deadline")???;
    run_case(&containers, &benchmark, case, target, image, &guest_program, deadline).await
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
) -> std::result::Result<MeasurementResult, Error> {
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
            Ok((elapsed, invocation_phases)) => {
                cleanup.map_err(|error| -> Error { error.into() })?;
                measurements.record(repetition, case.warmups, elapsed, invocation_phases)?;
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
    name: String,
    spec: ContainerSpec,
}

impl<'a> Invocation<'a> {
    fn new(
        containers: &'a hl_container::Containers,
        benchmark: &Benchmark,
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
            name,
            spec,
        })
    }

    async fn execute(&self, expected_stdout: &[u8]) -> std::result::Result<(u128, Vec<(String, u128, u64)>), Error> {
        let started = Instant::now();
        self.containers.create(self.spec.clone()).await?;
        let mut output = self.containers.attach(&self.name).await?;
        self.containers.start(&self.name).await?;
        let status = self.wait(&mut output).await?;
        let elapsed = started.elapsed().as_millis();
        let logs = self.containers.logs(&self.name).await?;
        bounded(&logs)?;
        if status != ExitStatus::Code(self.case.exit) {
            return Err(format!("exit {status:?}, expected {}", self.case.exit).into());
        }
        if expected_stdout.is_empty() && !logs.stdout.is_empty() {
            return Err(format!("expected empty stdout; stdout={}", output_excerpt(&logs.stdout)).into());
        }
        if !expected_stdout.is_empty()
            && !logs
                .stdout
                .windows(expected_stdout.len())
                .any(|window| window == expected_stdout)
        {
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
        Ok((elapsed, parse_phases(&logs.stdout)?))
    }
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
}

impl Measurements {
    fn new(samples: u32) -> Self {
        Self {
            cold: None,
            samples: Vec::with_capacity(samples as usize),
            phases: BTreeMap::new(),
        }
    }

    fn record(
        &mut self,
        repetition: u32,
        warmups: u32,
        elapsed: u128,
        phases: Vec<(String, u128, u64)>,
    ) -> std::result::Result<(), Error> {
        if repetition == 0 {
            self.cold = Some(elapsed);
            return Ok(());
        }
        if repetition <= warmups {
            return Ok(());
        }
        self.samples.push(elapsed);
        for (name, time, checksum) in phases {
            self.record_phase(&name, time, checksum)?;
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

    fn finish(self, samples: u32) -> std::result::Result<MeasurementResult, Error> {
        if self.phases.values().any(|(_, times)| times.len() != samples as usize) {
            return Err("PHASE set changed across samples".into());
        }
        Ok((
            self.cold.ok_or("cold benchmark sample was not run")?,
            self.samples,
            self.phases
                .into_iter()
                .map(|(name, (_, times))| (name, times))
                .collect(),
        ))
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
        CAPTURE_LIMIT, DIAGNOSTIC_OUTPUT, bounded, capture_size, isolated_state, output_excerpt, parse_phases,
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
