use super::{
    Error,
    definition::{Scenario, ScenarioCase},
};
use crate::runtime::{definition::Target, image::TestImage};
use hl_container::{Config, ContainerSpec, ExitStatus, Isolation, Process, Sandbox};
use std::{fs, time::Duration};

const IMAGE_TIMEOUT: Duration = Duration::from_secs(600);

pub enum CaseResult {
    Passed(String),
    Failed(String, String),
}

pub async fn run(scenario: &Scenario, target: Target) -> Result<Vec<CaseResult>, Error> {
    let state = tempfile::tempdir()?;
    let containers = hl_container::Containers::builder(Config::new(state.path()))
        .build()
        .await?;
    let mut results = Vec::new();
    for case in &scenario.cases {
        println!("RUN {} {}", case.id, target.name());
        let outcome = run_case(&containers, scenario, case, target).await;
        results.push(match outcome {
            Ok(()) => CaseResult::Passed(case.id.clone()),
            Err(error) => CaseResult::Failed(case.id.clone(), error.to_string()),
        });
    }
    Ok(results)
}

async fn run_case(
    containers: &hl_container::Containers,
    scenario: &Scenario,
    case: &ScenarioCase,
    target: Target,
) -> Result<(), Error> {
    let image = tokio::time::timeout(IMAGE_TIMEOUT, TestImage::materialize(&case.image, &target.platform()))
        .await
        .map_err(|_| {
            format!(
                "image {} did not materialize within {} seconds",
                case.image,
                IMAGE_TIMEOUT.as_secs()
            )
        })??;
    for fixture in &case.fixtures {
        let destination = image.path().join(fixture.destination.trim_start_matches('/'));
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&fixture.source, destination)?;
    }

    let name = format!(
        "testing-{}-{}-{}",
        scenario.name,
        target.name(),
        case.id.replace('/', "-")
    );
    let process = Process::new(&case.program)
        .args(case.arguments.iter().map(String::as_str))
        .working_dir(&case.working_directory);
    let spec = ContainerSpec::from_directory(image.path(), process)
        .name(&name)
        .guest(target.guest())
        .isolation(Isolation {
            sandbox: Sandbox::Disabled,
            ..Isolation::default()
        });

    let outcome = async {
        containers.create(spec).await?;
        containers.start(&name).await?;
        let status = tokio::time::timeout(Duration::from_secs(case.timeout), containers.wait(&name))
            .await
            .map_err(|_| format!("timed out after {} seconds", case.timeout))??;
        let logs = containers.logs(&name).await?;
        if status != ExitStatus::Code(case.exit) {
            return Err(format!("exit {status:?}, expected {}", case.exit).into());
        }
        let expected = fs::read(&case.stdout_contains)?;
        if !logs.stdout.windows(expected.len()).any(|window| window == expected) {
            return Err(format!("stdout does not contain {:?}: {:?}", expected, logs.stdout).into());
        }
        Ok::<(), Error>(())
    }
    .await;

    let cleanup = containers.remove_force(&name).await;
    let release = image.release();
    outcome?;
    cleanup?;
    release?;
    Ok(())
}
