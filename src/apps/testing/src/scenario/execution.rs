use super::{
    Error,
    definition::{Scenario, ScenarioAction, ScenarioCase},
};
use crate::{runtime::image::TestImage, suite::Target};
use hl_container::{Config, ContainerSpec, ExitStatus, Isolation, Process, Sandbox};
use std::{fs, time::Duration};

const IMAGE_TIMEOUT: Duration = Duration::from_secs(600);

pub enum CaseResult {
    Passed(String),
    Failed(String, String),
    ExpectedFailure(String, String),
    UnexpectedPass(String),
}

pub async fn run(scenario: &Scenario, target: Target) -> Result<Vec<CaseResult>, Error> {
    let state = tempfile::tempdir()?;
    let containers = hl_container::Containers::builder(Config::new(state.path()))
        .build()
        .await?;
    let mut results = Vec::new();
    for case in &scenario.cases {
        if !case.supports(target) {
            continue;
        }
        println!("RUN {} {}", case.id, target.name());
        let outcome = run_case(&containers, scenario, case, target).await;
        results.push(classify(&case.id, outcome, case.expects_failure(target)));
    }
    Ok(results)
}

fn classify(id: &str, outcome: Result<(), Error>, expected_failure: bool) -> CaseResult {
    match (outcome, expected_failure) {
        (Ok(()), false) => CaseResult::Passed(id.to_owned()),
        (Err(error), true) => CaseResult::ExpectedFailure(id.to_owned(), error.to_string()),
        (Ok(()), true) => CaseResult::UnexpectedPass(id.to_owned()),
        (Err(error), false) => CaseResult::Failed(id.to_owned(), error.to_string()),
    }
}

async fn run_case(
    containers: &hl_container::Containers,
    scenario: &Scenario,
    case: &ScenarioCase,
    target: Target,
) -> Result<(), Error> {
    if case.actions.len() != 1 {
        return Err(format!(
            "{} ordered actions are parsed but execution requires one action",
            case.id
        )
        .into());
    }
    if !case.resources.is_empty() {
        return Err(format!(
            "{} declares resources but no resource admission adapter is installed",
            case.id
        )
        .into());
    }
    if let Some(readiness) = &case.readiness {
        return Err(format!(
            "{} readiness requires the ordered exec adapter (startup_bytes={} probe_bytes={} attempts={} delay_ms={} logs={})",
            case.id,
            readiness.startup.len(),
            readiness.probe.len(),
            readiness.attempts,
            readiness.delay_ms,
            readiness.logs.len(),
        )
        .into());
    }
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
    let mut process = match &case.actions[0] {
        ScenarioAction::Argv(argv) => {
            let (program, arguments) = argv.split_first().ok_or("argv action is empty")?;
            Process::new(program).args(arguments.iter().map(String::as_str))
        }
        ScenarioAction::Shell(script) => Process::new("/bin/sh").args(["-c", script]),
        ScenarioAction::Entrypoint => {
            return Err(format!("{} entrypoint execution requires image runtime metadata", case.id).into());
        }
        ScenarioAction::Host(script) => {
            return Err(format!(
                "{} host action requires a typed host adapter (script_bytes={})",
                case.id,
                script.len()
            )
            .into());
        }
        ScenarioAction::Api(operation) => {
            return Err(format!(
                "{} API action requires a typed daemon adapter (operation={operation:?})",
                case.id
            )
            .into());
        }
    }
    .working_dir(&case.working_directory);
    for (name, value) in &case.environment {
        process = process.env(name, value);
    }
    let spec = ContainerSpec::from_directory(image.path(), process)
        .name(&name)
        .guest(target.guest())
        .execution(case.execution.container()?)
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
            return Err(format!(
                "exit {status:?}, expected {}; stdout={:?}; stderr={:?}",
                case.exit,
                String::from_utf8_lossy(&logs.stdout),
                String::from_utf8_lossy(&logs.stderr),
            )
            .into());
        }
        for path in &case.stdout_contains {
            let expected = fs::read(path)?;
            if !logs.stdout.windows(expected.len()).any(|window| window == expected) {
                return Err(format!(
                    "stdout does not contain {:?}; stdout={:?}; stderr={:?}",
                    String::from_utf8_lossy(&expected),
                    String::from_utf8_lossy(&logs.stdout),
                    String::from_utf8_lossy(&logs.stderr),
                )
                .into());
            }
        }
        if let Some(path) = &case.stdout_exact {
            let expected = fs::read(path)?;
            if logs.stdout != expected {
                return Err(format!(
                    "stdout differs from {}; stdout={:?}; stderr={:?}",
                    path.display(),
                    String::from_utf8_lossy(&logs.stdout),
                    String::from_utf8_lossy(&logs.stderr),
                )
                .into());
            }
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

#[cfg(test)]
mod tests {
    use super::{CaseResult, classify};

    #[test]
    fn expected_failures_remain_visible() {
        assert!(matches!(
            classify("case", Err("refused".into()), true),
            CaseResult::ExpectedFailure(id, reason) if id == "case" && reason == "refused"
        ));
        assert!(matches!(
            classify("case", Ok(()), true),
            CaseResult::UnexpectedPass(id) if id == "case"
        ));
    }
}
