use super::{
    Error,
    definition::{Scenario, ScenarioCase},
};
use crate::{runtime::image::TestImage, suite::Target};
use hl_container::{Config, ContainerSpec, ExecSpec, ExitStatus, Stream};
use hl_images::RuntimeConfig;
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
    let outcome = execute_image(scenario, case, target, image.path(), image.runtime()).await;
    let release = image.release();
    combine(
        outcome,
        release.map_err(|error| format!("image release failed: {error}")),
    )
}

fn validate_supported(case: &ScenarioCase) -> Result<(), Error> {
    let entrypoints = case
        .actions
        .iter()
        .filter(|action| matches!(action, super::definition::ScenarioAction::Entrypoint))
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
    scenario: &Scenario,
    case: &ScenarioCase,
    target: Target,
    image: &std::path::Path,
    runtime: &RuntimeConfig,
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
    let spec = specification(case, target, image, runtime, &name)?;
    let timeout = Duration::from_secs(case.timeout);
    let outcome = tokio::time::timeout(timeout, execute(&containers, case, runtime, image, spec, &name))
        .await
        .map_err(|_| format!("timed out after {} milliseconds", timeout.as_millis()))
        .and_then(|result| result.map_err(|error| error.to_string()));
    let cleanup = tokio::time::timeout(CLEANUP_TIMEOUT, containers.remove_force(&name))
        .await
        .map_err(|_| format!("cleanup timed out after {} seconds", CLEANUP_TIMEOUT.as_secs()))
        .and_then(|result| result.map_err(|error| error.to_string()));
    combine(
        outcome.map_err(Into::into),
        cleanup.map(|_| ()).map_err(|error| format!("cleanup failed: {error}")),
    )
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

async fn execute(
    containers: &hl_container::Containers,
    case: &ScenarioCase,
    runtime: &RuntimeConfig,
    rootfs: &std::path::Path,
    spec: ContainerSpec,
    name: &str,
) -> Result<(), Error> {
    containers.create(spec).await?;
    containers.start(name).await?;
    if matches!(
        case.actions.first(),
        Some(super::definition::ScenarioAction::Entrypoint)
    ) {
        let status = wait(containers, name, Duration::from_secs(case.timeout)).await?;
        let logs = containers.logs(name).await?;
        bounded(&logs)?;
        return verify(case, status, &logs.stdout, &logs.stderr);
    }
    if let Some(readiness) = &case.readiness {
        let startup = run_exec(
            containers,
            case,
            runtime,
            rootfs,
            name,
            &super::definition::ScenarioAction::Shell(readiness.startup.clone()),
        )
        .await?;
        require_success("readiness startup", startup.0, &startup.1, &startup.2)?;
        let mut ready = false;
        for attempt in 1..=readiness.attempts {
            let probe = run_exec(
                containers,
                case,
                runtime,
                rootfs,
                name,
                &super::definition::ScenarioAction::Shell(readiness.probe.clone()),
            )
            .await?;
            if probe.0 == ExitStatus::Code(0) {
                ready = true;
                break;
            }
            if attempt < readiness.attempts {
                tokio::time::sleep(Duration::from_millis(readiness.delay_ms)).await;
            }
        }
        if !ready {
            return Err(format!(
                "readiness failed after {} attempts; service logs: {}",
                readiness.attempts,
                readiness_logs(rootfs, &readiness.logs)
            )
            .into());
        }
    }
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut status = ExitStatus::Code(0);
    for action in &case.actions {
        let outcome = run_exec(containers, case, runtime, rootfs, name, action).await?;
        status = outcome.0;
        stdout.extend(outcome.1);
        stderr.extend(outcome.2);
        bounded_size(stdout.len(), stderr.len())?;
        if status != ExitStatus::Code(0) {
            break;
        }
    }
    verify(case, status, &stdout, &stderr)
}

async fn run_exec(
    containers: &hl_container::Containers,
    case: &ScenarioCase,
    runtime: &RuntimeConfig,
    rootfs: &std::path::Path,
    name: &str,
    action: &super::definition::ScenarioAction,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), Error> {
    let process = super::process::action(case, action, runtime, rootfs)?;
    let execution = containers.executions().create(name, ExecSpec::new(process)).await?;
    let mut session = containers.executions().start(&execution.id).await?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    while let Some(entry) = session.next().await? {
        match entry.stream {
            Stream::Stdout => stdout.extend(entry.bytes),
            Stream::Stderr => stderr.extend(entry.bytes),
        }
        bounded_size(stdout.len(), stderr.len())?;
    }
    let status = containers.executions().wait(&execution.id).await?;
    Ok((status, stdout, stderr))
}

fn require_success(noun: &str, status: ExitStatus, stdout: &[u8], stderr: &[u8]) -> Result<(), Error> {
    if status != ExitStatus::Code(0) {
        return Err(format!("{noun} exited {status:?}; {}", output_summary(stdout, stderr)).into());
    }
    Ok(())
}

fn verify(case: &ScenarioCase, status: ExitStatus, stdout: &[u8], stderr: &[u8]) -> Result<(), Error> {
    if status != ExitStatus::Code(case.exit) {
        return Err(format!(
            "exit {status:?}, expected {}; {}",
            case.exit,
            output_summary(stdout, stderr)
        )
        .into());
    }
    let mut output = Vec::with_capacity(stdout.len().saturating_add(stderr.len()));
    output.extend_from_slice(stdout);
    output.extend_from_slice(stderr);
    for path in &case.stdout_contains {
        let expected = fs::read(path)?;
        if !expected.is_empty() && !output.windows(expected.len()).any(|window| window == expected) {
            return Err(format!(
                "stdout does not contain {:?}; {}",
                String::from_utf8_lossy(&expected),
                output_summary(stdout, stderr)
            )
            .into());
        }
    }
    if let Some(path) = &case.stdout_exact {
        let expected = fs::read(path)?;
        if output != expected {
            return Err(format!(
                "combined output differs from {}; {}",
                path.display(),
                output_summary(stdout, stderr)
            )
            .into());
        }
    }
    Ok(())
}

fn readiness_logs(rootfs: &std::path::Path, paths: &[String]) -> String {
    paths
        .iter()
        .take(8)
        .map(|path| {
            let relative = std::path::Path::new(path)
                .strip_prefix("/")
                .unwrap_or_else(|_| std::path::Path::new(path));
            if relative
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return format!("{path}=<invalid path>");
            }
            match fs::read(rootfs.join(relative)) {
                Ok(bytes) => format!("{path}={:?}", String::from_utf8_lossy(excerpt(&bytes))),
                Err(error) => format!("{path}=<unavailable: {error}>"),
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
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

fn output_summary(stdout: &[u8], stderr: &[u8]) -> String {
    format!("stdout={:?}; stderr={:?}", excerpt(stdout), excerpt(stderr))
}

fn combine(primary: Result<(), Error>, secondary: Result<(), String>) -> Result<(), Error> {
    match (primary, secondary) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), Err(secondary)) => Err(secondary.into()),
        (Err(primary), Err(secondary)) => Err(format!("{primary}; {secondary}").into()),
    }
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
    use super::{CAPTURE_LIMIT, CaseResult, bounded_size, classify, combine, diagnostic, verify};
    use crate::{
        scenario::definition::{Class, ScenarioAction, ScenarioCase},
        suite::{Execution, Target},
    };
    use hl_container::ExitStatus;
    use std::collections::BTreeMap;

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

    #[test]
    fn verification_combines_stdout_and_stderr() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("marker.txt");
        std::fs::write(&marker, b"from-stderr").unwrap();
        let case = ScenarioCase {
            id: "example/output".into(),
            image: "fixture".into(),
            execution: Execution::default(),
            class: Class::Quick,
            targets: vec![Target::Arm64],
            expected_failures: Vec::new(),
            resources: Vec::new(),
            environment: BTreeMap::new(),
            working_directory: "/".into(),
            actions: vec![ScenarioAction::Shell("true".into())],
            fixtures: Vec::new(),
            readiness: None,
            timeout: 1,
            exit: 0,
            stdout_contains: vec![marker],
            stdout_exact: None,
        };
        assert!(verify(&case, ExitStatus::Code(0), b"stdout", b"from-stderr").is_ok());
    }

    #[test]
    fn cleanup_errors_are_not_lost_after_primary_failure() {
        let error = combine(Err("execution failed".into()), Err("cleanup failed".into())).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("execution failed"));
        assert!(message.contains("cleanup failed"));
    }
}
