use super::{process::ProcessGroup, Error};
use crate::contract::Target;
use crate::report::{Status, Store, WorkflowAttempt, WorkflowKey, WorkflowOutcome};
use std::{
    collections::BTreeMap,
    env,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{process::Command, sync::Semaphore, task::JoinSet};

pub(super) async fn run(
    executable: &Path,
    jobs: usize,
    cache: &Path,
    run: &str,
    resume: bool,
    target: Target,
) -> Result<Vec<&'static str>, Error> {
    let report_base = env::var_os("HL_SCENARIO_REPORT_DIR").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("hl-daemon belongs to a workspace")
                .join(".cache/scenario-runs")
        },
        PathBuf::from,
    );
    let store = Arc::new(Store::create(&report_base, run)?);
    let archive = env::var("HL_ENGINE_ARCHIVE_SHA256").unwrap_or_else(|_| "unknown".into());
    let recorded = if resume {
        store.resume_workflows()?
    } else {
        BTreeMap::new()
    };
    let permits = Arc::new(Semaphore::new(jobs));
    let mut tasks = JoinSet::new();
    let mut failures = Vec::new();
    for workflow in crate::workflows::NAMES {
        let key = WorkflowKey {
            workflow: workflow.into(),
            engine_archive_hash: archive.clone(),
        };
        if let Some(outcome) = recorded
            .get(&key)
            .filter(|outcome| outcome.status != Status::InfrastructureFail)
        {
            eprintln!("RESUME [workflow] {workflow} {:?}", outcome.status);
            if outcome.status != Status::Pass {
                failures.push(workflow);
            }
            continue;
        }
        let permit = permits.clone().acquire_owned().await?;
        let executable = executable.to_owned();
        let cache = cache.to_owned();
        let run = run.to_owned();
        let store = store.clone();
        tasks.spawn(async move {
            let _permit = permit;
            let started = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            store.begin_workflow(&WorkflowAttempt {
                key: key.clone(),
                started_at: started.as_millis().to_string(),
            })?;
            let timer = Instant::now();
            eprintln!("START [workflow] {workflow}");
            let mut command = command(&executable, &cache, &run, target, workflow);
            let completion = match ProcessGroup::spawn(&mut command) {
                Ok(process) => process.wait().await,
                Err(error) => Err(error),
            };
            let (status, process_exit, error) = match completion {
                Ok(completion) if completion.success() => {
                    eprintln!("DONE  [workflow] {workflow} {completion}");
                    (Status::Pass, completion.code(), None)
                }
                Ok(completion) => {
                    eprintln!("DONE  [workflow] {workflow} {completion}");
                    (
                        Status::RuntimeFail,
                        completion.code(),
                        Some(completion.to_string()),
                    )
                }
                Err(error) => (Status::InfrastructureFail, None, Some(error.to_string())),
            };
            let passed = status == Status::Pass;
            store.append_workflow(&WorkflowOutcome {
                key,
                started_at: started.as_millis().to_string(),
                duration_ms: timer.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                status,
                process_exit,
                error,
            })?;
            Ok::<_, Error>((workflow, passed))
        });
    }
    while let Some(result) = tasks.join_next().await {
        let (workflow, passed) = result??;
        if !passed {
            failures.push(workflow);
        }
    }
    Ok(failures)
}

fn command(executable: &Path, cache: &Path, run: &str, target: Target, workflow: &str) -> Command {
    let mut command = Command::new(executable);
    command
        .args(["workflow", workflow])
        .env("HL_SCENARIO_RUN_ID", run)
        .env("HL_SCENARIO_CHILD", "1")
        .env("HL_SCENARIO_IMAGE_CACHE", cache)
        .env("HL_SCENARIO_TARGET", target.name())
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command
}

pub(super) fn test_target_cache() {
    let command = command(
        Path::new("/scenario-runner"),
        Path::new("/cache/amd64"),
        "run-1",
        Target::Amd64,
        "realsw",
    );
    let command = command.as_std();
    let value = |expected: &str| {
        command
            .get_envs()
            .find(|(name, _)| *name == expected)
            .and_then(|(_, value)| value)
    };
    assert_eq!(
        value("HL_SCENARIO_IMAGE_CACHE"),
        Some(std::ffi::OsStr::new("/cache/amd64"))
    );
    assert_eq!(
        value("HL_SCENARIO_TARGET"),
        Some(std::ffi::OsStr::new("amd64"))
    );
}
