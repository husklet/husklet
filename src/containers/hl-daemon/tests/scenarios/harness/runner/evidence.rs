use std::{fs, path::Path, time::Duration};

use crate::{
    contract::{Scenario, Service, Target},
    fixture::Fixture,
    report::{ScenarioKey, ScenarioOutcome, Status, Store},
};
use hl_container::ExitStatus;

use super::{Error, SERVICE_LOG_LIMIT};

pub(super) struct CommandOutcome {
    pub(super) status: ExitStatus,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
    pub(super) timed_out: bool,
}

pub(super) fn exit_code(status: ExitStatus) -> String {
    match status {
        ExitStatus::Code(code) => code.to_string(),
        _ => "none".into(),
    }
}

pub(super) fn signal(status: ExitStatus) -> String {
    match status {
        ExitStatus::Signal(signal) => signal.to_string(),
        _ => "none".into(),
    }
}

pub(super) fn service_logs(fixture: &Fixture, service: &Service) -> String {
    bounded_service_logs(fixture.path(), service)
}

fn bounded_service_logs(root: &Path, service: &Service) -> String {
    service
        .logs
        .iter()
        .take(8)
        .map(|path| {
            let relative = path.strip_prefix('/').unwrap_or(path);
            if relative.split('/').any(|part| part == "..") {
                return format!("{path}=<invalid path>");
            }
            let file = root.join(relative);
            match fs::File::open(&file) {
                Ok(file) => {
                    use std::io::Read;
                    let mut bytes = Vec::new();
                    let _ = file.take(SERVICE_LOG_LIMIT).read_to_end(&mut bytes);
                    format!("{path}={:?}", String::from_utf8_lossy(&bytes))
                }
                Err(error) => format!("{path}=<unavailable: {error}>"),
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub(crate) fn test_service_diagnostics() -> Result<(), Error> {
    let root = tempfile::tempdir()?;
    let log_limit = usize::try_from(SERVICE_LOG_LIMIT).expect("service log limit fits in usize");
    fs::create_dir(root.path().join("tmp"))?;
    fs::write(
        root.path().join("tmp/server.log"),
        vec![b'x'; log_limit + 17],
    )?;
    let service = Service {
        startup: "server &".into(),
        probe: "probe".into(),
        attempts: 3,
        delay_ms: 1,
        logs: vec!["/tmp/server.log".into(), "../secret".into()],
    };

    let output = bounded_service_logs(root.path(), &service);

    assert!(output.contains("/tmp/server.log="));
    assert!(output.contains("../secret=<invalid path>"));
    assert_eq!(output.matches('x').count(), log_limit);
    let case = Scenario::new("service/deadline", "fixture").timeout(240);
    assert_eq!(case.operation_timeout(), Duration::from_secs(240));
    println!("PASS service-diagnostics-test");
    Ok(())
}

pub(super) fn report_outcome(
    case: &Scenario,
    key: ScenarioKey,
    status: Status,
    error: Option<String>,
    duration: Duration,
    started: Duration,
    store: &Store,
    target: Target,
) -> ScenarioOutcome {
    ScenarioOutcome {
        key,
        category: case.id.split('/').next().unwrap_or("other").into(),
        declared_image: case.image.into(),
        resolved_digest: None,
        step: serde_json::to_value(&case.step).unwrap_or_default(),
        timeout_seconds: case.timeout_seconds,
        checks: case.checks.iter().map(|v| format!("{v:?}")).collect(),
        started_at: started.as_millis().to_string(),
        duration_ms: duration.as_millis().try_into().unwrap_or(u64::MAX),
        status,
        process_exit: None,
        process_signal: None,
        expected_failure: case.expected_failures.contains(&target),
        error,
        log_path: store.log_path(case.id).display().to_string(),
    }
}
