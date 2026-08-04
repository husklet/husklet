use super::{
    Error,
    definition::{Scenario, ScenarioAction, ScenarioCase},
};
use crate::{runtime::image::TestImage, suite::Target};
use hl_container::{Config, ContainerSpec, ExitStatus, Isolation, Process, Sandbox};
use std::{fs, sync::Arc, time::Duration};
use tokio::time::Instant;

const IMAGE_TIMEOUT: Duration = Duration::from_secs(600);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);
const CAPTURE_LIMIT: usize = 1024 * 1024;
const DIAGNOSTIC_LIMIT: usize = 4096;

pub enum CaseResult {
    Passed,
    Failed(String),
    ExpectedFailure(String),
    UnexpectedPass,
}

impl CaseResult {
    pub(super) fn evidence(&self) -> (&'static str, String) {
        match self {
            Self::Passed => ("pass", String::new()),
            Self::Failed(error) => ("fail", diagnostic(error)),
            Self::ExpectedFailure(error) => ("xfail", diagnostic(error)),
            Self::UnexpectedPass => ("xpass", "unexpected pass".to_owned()),
        }
    }
}

pub async fn run_case(scenario: Arc<Scenario>, case_index: usize, target: Target) -> CaseResult {
    let case = &scenario.cases[case_index];
    let outcome = execute_case(&scenario, case, target).await;
    classify(outcome, case.expects_failure(target))
}

fn classify(outcome: Result<(), Error>, expected_failure: bool) -> CaseResult {
    match (outcome, expected_failure) {
        (Ok(()), false) => CaseResult::Passed,
        (Err(error), true) => CaseResult::ExpectedFailure(error.to_string()),
        (Ok(()), true) => CaseResult::UnexpectedPass,
        (Err(error), false) => CaseResult::Failed(error.to_string()),
    }
}

async fn execute_case(scenario: &Scenario, case: &ScenarioCase, target: Target) -> Result<(), Error> {
    validate_supported(case)?;
    let image = tokio::time::timeout(IMAGE_TIMEOUT, TestImage::materialize(&case.image, &target.platform()))
        .await
        .map_err(|_| {
            format!(
                "image {} did not materialize within {} seconds",
                case.image,
                IMAGE_TIMEOUT.as_secs()
            )
        })??;
    let outcome = execute_image(scenario, case, target, image.path()).await;
    let release = image.release();
    match (outcome, release) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(format!("image release failed: {error}").into()),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn validate_supported(case: &ScenarioCase) -> Result<(), Error> {
    if case.actions.len() != 1 {
        return Err(format!(
            "{} ordered actions are parsed but execution requires one action",
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
    Ok(())
}

async fn execute_image(
    scenario: &Scenario,
    case: &ScenarioCase,
    target: Target,
    image: &std::path::Path,
) -> Result<(), Error> {
    install_fixtures(case, image)?;
    let state = tempfile::tempdir()?;
    let containers = hl_container::Containers::builder(Config::new(state.path()))
        .build()
        .await?;
    let name = format!(
        "testing-{}-{}-{}",
        scenario.name,
        target.name(),
        case.id.replace('/', "-")
    );
    let spec = specification(case, target, image, &name)?;
    let timeout = Duration::from_secs(case.timeout);
    let outcome = tokio::time::timeout(timeout, execute(&containers, case, spec, &name))
        .await
        .map_err(|_| format!("timed out after {} milliseconds", timeout.as_millis()))
        .and_then(|result| result.map_err(|error| error.to_string()));
    let cleanup = tokio::time::timeout(CLEANUP_TIMEOUT, containers.remove_force(&name))
        .await
        .map_err(|_| format!("cleanup timed out after {} seconds", CLEANUP_TIMEOUT.as_secs()))
        .and_then(|result| result.map_err(|error| error.to_string()));
    match (outcome, cleanup) {
        (Err(error), _) => Err(error.into()),
        (Ok(()), Err(error)) => Err(format!("cleanup failed: {error}").into()),
        (Ok(()), Ok(_)) => Ok(()),
    }
}

fn install_fixtures(case: &ScenarioCase, image: &std::path::Path) -> Result<(), Error> {
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
    case: &ScenarioCase,
    target: Target,
    image: &std::path::Path,
    name: &str,
) -> Result<ContainerSpec, Error> {
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
    Ok(ContainerSpec::from_directory(image, process)
        .name(name)
        .guest(target.guest())
        .execution(case.execution.container()?)
        .isolation(Isolation {
            sandbox: Sandbox::Disabled,
            ..Isolation::default()
        }))
}

async fn execute(
    containers: &hl_container::Containers,
    case: &ScenarioCase,
    spec: ContainerSpec,
    name: &str,
) -> Result<(), Error> {
    containers.create(spec).await?;
    containers.start(name).await?;
    let status = wait(containers, name, Duration::from_secs(case.timeout)).await?;
    let logs = containers.logs(name).await?;
    bounded(&logs)?;
    if status != ExitStatus::Code(case.exit) {
        return Err(format!("exit {status:?}, expected {}; {}", case.exit, log_summary(&logs)).into());
    }
    for path in &case.stdout_contains {
        let expected = fs::read(path)?;
        if !logs.stdout.windows(expected.len()).any(|window| window == expected) {
            return Err(format!(
                "stdout does not contain {:?}; {}",
                String::from_utf8_lossy(&expected),
                log_summary(&logs)
            )
            .into());
        }
    }
    if let Some(path) = &case.stdout_exact {
        let expected = fs::read(path)?;
        if logs.stdout != expected {
            return Err(format!("stdout differs from {}; {}", path.display(), log_summary(&logs)).into());
        }
    }
    Ok(())
}

async fn wait(containers: &hl_container::Containers, name: &str, timeout: Duration) -> Result<ExitStatus, Error> {
    let waiting = containers.wait(name);
    tokio::pin!(waiting);
    let deadline = Instant::now() + timeout;
    loop {
        tokio::select! {
            result = &mut waiting => return Ok(result?),
            () = tokio::time::sleep_until(deadline) => {
                return Err(format!("timed out after {} milliseconds", timeout.as_millis()).into());
            }
            () = tokio::time::sleep(Duration::from_millis(10)) => {
                bounded(&containers.logs(name).await?)?;
            }
        }
    }
}

fn bounded(logs: &hl_container::Logs) -> Result<(), Error> {
    bounded_size(logs.stdout.len(), logs.stderr.len())
}

fn bounded_size(stdout: usize, stderr: usize) -> Result<(), Error> {
    let size = stdout.checked_add(stderr).ok_or("captured output size overflow")?;
    if size > CAPTURE_LIMIT {
        Err(format!("captured output exceeded {CAPTURE_LIMIT} bytes").into())
    } else {
        Ok(())
    }
}

fn log_summary(logs: &hl_container::Logs) -> String {
    format!("stdout={:?}; stderr={:?}", excerpt(&logs.stdout), excerpt(&logs.stderr))
}

fn excerpt(bytes: &[u8]) -> &[u8] {
    &bytes[..bytes.len().min(DIAGNOSTIC_LIMIT)]
}

fn diagnostic(error: &str) -> String {
    let mut result = String::with_capacity(error.len().min(DIAGNOSTIC_LIMIT));
    for character in error.chars() {
        let character = match character {
            '\t' | '\n' | '\r' => ' ',
            value => value,
        };
        if result.len() + character.len_utf8() > DIAGNOSTIC_LIMIT {
            break;
        }
        result.push(character);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{CAPTURE_LIMIT, CaseResult, bounded_size, classify, diagnostic};

    #[test]
    fn expected_failures_remain_visible() {
        assert!(matches!(
            classify(Err("refused".into()), true),
            CaseResult::ExpectedFailure(reason) if reason == "refused"
        ));
        assert!(matches!(classify(Ok(()), true), CaseResult::UnexpectedPass));
    }

    #[test]
    fn combined_capture_is_bounded() {
        assert!(bounded_size(CAPTURE_LIMIT - 1, 1).is_ok());
        assert!(bounded_size(CAPTURE_LIMIT, 1).is_err());
        assert!(bounded_size(usize::MAX, 1).is_err());
    }

    #[test]
    fn durable_diagnostics_are_bounded() {
        assert_eq!(diagnostic(&"x".repeat(5000)).len(), 4096);
        assert_eq!(diagnostic("line\tone\nline two"), "line one line two");
        assert!(diagnostic(&"🙂".repeat(5000)).len() <= 4096);
    }
}
