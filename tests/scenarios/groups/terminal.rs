//! PTY, devpts, termios, window-size, shell, and job-control compatibility.

use super::contract;
use crate::report::{Attempt, BatchMetadata, BatchReport, ScenarioKey, ScenarioOutcome, Status, Store};
use hl_container::{Console, ContainerSpec, Containers, ExitStatus, Isolation, Process, Sandbox, Size};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

type Error = Box<dyn std::error::Error>;

pub(crate) fn group() -> contract::Group {
    contract::Group::new(
        "terminal",
        crate::manifest::load(include_str!("../fixtures/terminal-core.yaml"))
            .expect("the checked-in terminal manifest must satisfy the schema"),
    )
}

#[allow(clippy::too_many_lines, reason = "PTY executor plus reporting lifecycle")]
pub(crate) async fn run(containers: &Containers) -> Result<(), Error> {
    let selected = std::env::var("HL_SCENARIO_CASE").ok();
    let cases = group()
        .scenarios
        .into_iter()
        .filter(|case| selected.as_deref().is_none_or(|id| case.id == id))
        .collect::<Vec<_>>();
    if cases.is_empty() {
        return Err(format!("terminal scenario {:?} is not registered", selected.unwrap_or_default()).into());
    }
    let store = Store::from_env()?;
    let archive = std::env::var("HL_ENGINE_ARCHIVE_SHA256").unwrap_or_else(|_| "unknown".into());
    let mut recorded = store.as_ref().map(Store::resume).transpose()?.unwrap_or_default();
    let mut failures = Vec::new();
    for case in &cases {
        let key = ScenarioKey {
            scenario: case.id.into(),
            target: "arm64".into(),
            image_digest: case.image.into(),
            engine_archive_hash: archive.clone(),
        };
        if recorded
            .get(&key)
            .is_some_and(|value| value.status != Status::InfrastructureFail)
        {
            println!("RESUME {}", case.id);
            continue;
        }
        let started = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let timer = Instant::now();
        if let Some(store) = &store {
            store.begin(&Attempt {
                key: key.clone(),
                started_at: started.as_millis().to_string(),
                log_path: store.log_path(case.id).display().to_string(),
            })?;
        }
        let result = execute(case, containers).await;
        if let Some(store) = &store {
            let error = result.as_ref().err().map(ToString::to_string);
            let status = if result.is_ok() {
                Status::Pass
            } else if error.as_deref().is_some_and(|value| value.contains("timed out")) {
                Status::Timeout
            } else {
                Status::RuntimeFail
            };
            let outcome = ScenarioOutcome {
                key,
                category: "terminal".into(),
                declared_image: case.image.into(),
                resolved_digest: None,
                step: serde_json::to_value(&case.step)?,
                timeout_seconds: case.timeout_seconds,
                checks: case.checks.iter().map(|value| format!("{value:?}")).collect(),
                started_at: started.as_millis().to_string(),
                duration_ms: timer.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                status,
                process_exit: None,
                process_signal: None,
                expected_failure: false,
                error,
                log_path: store.log_path(case.id).display().to_string(),
            };
            store.append(&outcome)?;
            Store::write_log(
                std::path::Path::new(&outcome.log_path),
                outcome.error.as_deref().unwrap_or("pass\n"),
            )?;
            recorded.insert(outcome.key.clone(), outcome);
        }
        match result {
            Ok(()) => println!("PASS {}", case.id),
            Err(error) => {
                println!("FAIL {}: {error}", case.id);
                failures.push(format!("{}: {error}", case.id));
            }
        }
    }
    println!(
        "terminal scenarios: {} passed; {} failed; {} total",
        cases.len() - failures.len(),
        failures.len(),
        cases.len()
    );
    if let Some(store) = &store {
        store.finish(&BatchReport::new(
            BatchMetadata {
                run_id: std::env::var("HL_SCENARIO_RUN_ID").unwrap_or_default(),
                started_unix_ms: 0,
                engine_archive_hash: archive,
                targets: vec!["arm64".into()],
                images: BTreeMap::new(),
                categories: vec!["terminal".into()],
                filters: selected.into_iter().collect(),
            },
            recorded.into_values().collect(),
            Vec::new(),
        ))?;
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n").into())
    }
}

async fn execute(case: &contract::Scenario, containers: &Containers) -> Result<(), Error> {
    let fixture = super::fixture::Fixture::materialize(case.image).await?;
    let name = case.id.replace('/', "-");
    let mut process = match &case.step {
        contract::Step::Exec(command) => Process::new("/bin/sh").args(["-c", command]),
        contract::Step::Run(arguments) if !arguments.is_empty() => {
            Process::new(&arguments[0]).args(arguments[1..].iter().cloned())
        }
        contract::Step::Run(_) => return Err("empty argv requires exact image entrypoint".into()),
        contract::Step::Host(command) => {
            return Err(format!("host step is not supported by terminal runner: {command}").into());
        }
        contract::Step::Api(operation) => {
            return Err(format!("API step is not supported by terminal runner: {operation}").into());
        }
    };
    for (key, value) in &fixture.runtime().environment {
        process = process.env(key, value);
    }
    for (key, value) in &case.environment {
        process = process.env(key, value);
    }
    if !fixture.runtime().working_directory.is_empty() {
        process = process.working_dir(&fixture.runtime().working_directory);
    }
    if case.terminal {
        if !fixture.runtime().environment.contains_key("TERM") && !case.environment.contains_key("TERM") {
            process = process.env("TERM", "xterm");
        }
        process = process.console(Console {
            stdin: false,
            terminal: Some(Size::new(24, 80)?),
        });
    }
    containers
        .create(
            ContainerSpec::from_directory(fixture.path(), process)
                .name(&name)
                .isolation(Isolation {
                    sandbox: Sandbox::Disabled,
                    ..Isolation::default()
                }),
        )
        .await?;
    containers.start(&name).await?;
    let status = tokio::time::timeout(Duration::from_secs(case.timeout_seconds), containers.wait(&name))
        .await
        .map_err(|_| format!("timed out after {} seconds", case.timeout_seconds))??;
    let logs = containers.logs(&name).await?;
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&logs.stdout),
        String::from_utf8_lossy(&logs.stderr)
    );
    let expected_exit = case
        .checks
        .iter()
        .find_map(|check| match check {
            contract::Check::Exit(code) => Some(*code),
            _ => None,
        })
        .unwrap_or(0);
    let checks = case.checks.iter().all(|check| match check {
        contract::Check::Contains(marker) => output.contains(marker),
        contract::Check::Equals(expected) => output == *expected,
        contract::Check::Exit(_) => true,
    });
    fixture.release()?;
    if status == ExitStatus::Code(expected_exit) && checks {
        Ok(())
    } else {
        Err(format!("status={status:?} checks={:?} output={output:?}", case.checks).into())
    }
}
