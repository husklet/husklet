use super::{
    Error,
    definition::{Benchmark, BenchmarkCase},
};
use crate::{runtime::image::TestImage, suite::Target};
use hl_container::{Config, ContainerSpec, ExitStatus, Isolation, Process, Sandbox};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
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

const CAPTURE_LIMIT: usize = 1024 * 1024;

pub async fn run(benchmark: &Benchmark, target: Target) -> std::result::Result<Vec<Result>, Error> {
    let image = TestImage::materialize(&benchmark.image, &target.platform()).await?;
    let state = tempfile::tempdir()?;
    let containers = hl_container::Containers::builder(Config::new(state.path()))
        .build()
        .await?;
    let mut results = Vec::new();
    for case in &benchmark.cases {
        let artifact = benchmark.build(case, target)?;
        let guest_program = format!("/opt/husklet/bench-{}", case.id);
        let destination = image.path().join(guest_program.trim_start_matches('/'));
        fs::create_dir_all(destination.parent().ok_or("benchmark destination has no parent")?)?;
        fs::copy(artifact, &destination)?;
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))?;
        results.push(
            match run_case(&containers, benchmark, case, target, image.path(), &guest_program).await {
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
            },
        );
    }
    image.release()?;
    Ok(results)
}

async fn run_case(
    containers: &hl_container::Containers,
    benchmark: &Benchmark,
    case: &BenchmarkCase,
    target: Target,
    image: &std::path::Path,
    program: &str,
) -> std::result::Result<(u128, Vec<u128>, BTreeMap<String, Vec<u128>>), Error> {
    let expected_stdout = fs::read(&case.stdout_contains)?;
    let total = 1_u32
        .checked_add(case.warmups)
        .and_then(|value| value.checked_add(case.samples))
        .ok_or("benchmark invocation count overflow")?;
    let mut measurements = Measurements::new(case.samples);
    for repetition in 0..total {
        let invocation = Invocation::new(containers, benchmark, case, target, image, program, repetition)?;
        let outcome = invocation.execute(&expected_stdout).await;
        let cleanup = containers.remove_force(&invocation.name).await;
        match outcome {
            Ok((elapsed, invocation_phases)) => {
                cleanup?;
                measurements.record(repetition, case.warmups, elapsed, invocation_phases)?;
            }
            Err(error) => {
                let _ = cleanup;
                return Err(error);
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
        self.containers.start(&self.name).await?;
        let status = tokio::time::timeout(Duration::from_secs(self.case.timeout), self.containers.wait(&self.name))
            .await
            .map_err(|_| format!("timed out after {} seconds", self.case.timeout))??;
        let elapsed = started.elapsed().as_millis();
        let logs = self.containers.logs(&self.name).await?;
        let captured = logs.stdout.len().saturating_add(logs.stderr.len());
        if captured > CAPTURE_LIMIT {
            return Err(format!("output exceeded {CAPTURE_LIMIT} bytes").into());
        }
        if status != ExitStatus::Code(self.case.exit) {
            return Err(format!("exit {status:?}, expected {}", self.case.exit).into());
        }
        if expected_stdout.is_empty() && !logs.stdout.is_empty() {
            return Err(format!("expected empty stdout; stdout={:?}", logs.stdout).into());
        }
        if !expected_stdout.is_empty()
            && !logs
                .stdout
                .windows(expected_stdout.len())
                .any(|window| window == expected_stdout)
        {
            return Err(format!(
                "stdout missing marker from {}; stdout={:?}",
                self.case.stdout_contains.display(),
                logs.stdout
            )
            .into());
        }
        if !logs.stderr.is_empty() {
            return Err(format!("unexpected stderr: {:?}", logs.stderr).into());
        }
        Ok((elapsed, parse_phases(&logs.stdout)?))
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
            self.record_phase(name, time, checksum)?;
        }
        Ok(())
    }

    fn record_phase(&mut self, name: String, time: u128, checksum: u64) -> std::result::Result<(), Error> {
        let phase = self
            .phases
            .entry(name.clone())
            .or_insert_with(|| (checksum, Vec::new()));
        if name != "syscall" && phase.0 != checksum {
            return Err(format!("PHASE {name} checksum changed across samples").into());
        }
        phase.1.push(time);
        Ok(())
    }

    fn finish(self, samples: u32) -> std::result::Result<(u128, Vec<u128>, BTreeMap<String, Vec<u128>>), Error> {
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
    use super::parse_phases;

    #[test]
    fn retained_phase_protocol_is_accepted() {
        let phases = parse_phases(b"noise\nPHASE compute us=42 ok=7\n").unwrap();
        assert_eq!(phases, vec![("compute".to_owned(), 42, 7)]);
        assert!(parse_phases(b"PHASE compute ms=42 ok=7\n").is_err());
    }
}
