use super::{
    Error,
    definition::{Benchmark, BenchmarkCase},
};
use crate::runtime::{definition::Target, image::TestImage};
use hl_container::{Config, ContainerSpec, ExitStatus, Isolation, Process, Sandbox};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    time::{Duration, Instant},
};

pub enum Result {
    Passed { id: String, samples: Vec<u128> },
    Failed { id: String, reason: String },
}

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
                Ok(samples) => Result::Passed {
                    id: format!("{}/{}", benchmark.name, case.id),
                    samples,
                },
                Err(error) => Result::Failed {
                    id: format!("{}/{}", benchmark.name, case.id),
                    reason: error.to_string(),
                },
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
) -> std::result::Result<Vec<u128>, Error> {
    let mut samples = Vec::new();
    let expected_stdout = fs::read(&case.stdout_contains)?;
    for repetition in 0..case.repetitions {
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
        let started = Instant::now();
        let outcome = async {
            containers.create(spec).await?;
            containers.start(&name).await?;
            let status = tokio::time::timeout(Duration::from_secs(case.timeout), containers.wait(&name))
                .await
                .map_err(|_| format!("timed out after {} seconds", case.timeout))??;
            let elapsed = started.elapsed().as_millis();
            let logs = containers.logs(&name).await?;
            if status != ExitStatus::Code(case.exit) {
                return Err(format!("exit {status:?}, expected {}", case.exit).into());
            }
            if expected_stdout.is_empty() {
                if !logs.stdout.is_empty() {
                    return Err(format!("expected empty stdout; stdout={:?}", logs.stdout).into());
                }
            } else if !logs
                .stdout
                .windows(expected_stdout.len())
                .any(|window| window == expected_stdout)
            {
                return Err(format!(
                    "stdout missing marker from {}; stdout={:?}",
                    case.stdout_contains.display(),
                    logs.stdout
                )
                .into());
            }
            if !logs.stderr.is_empty() {
                return Err(format!("unexpected stderr: {:?}", logs.stderr).into());
            }
            Ok::<u128, Error>(elapsed)
        }
        .await;
        let cleanup = containers.remove_force(&name).await;
        match outcome {
            Ok(elapsed) => {
                cleanup?;
                samples.push(elapsed);
            }
            Err(error) => {
                let _ = cleanup;
                return Err(error);
            }
        }
    }
    Ok(samples)
}
