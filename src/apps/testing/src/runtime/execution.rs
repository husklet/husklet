mod worker;

use super::{Error, definition::App, image::TestImage};
use crate::suite::{BoundedCapture as _, Target};
use hl_container::{Config, ContainerSpec, Containers, ExitStatus, Isolation, Process, Sandbox};
use serde::{Deserialize, Serialize};
use std::{fs, sync::Arc, time::Duration};
use tokio::time::Instant;

pub(crate) use worker::Options as WorkerOptions;


#[derive(Clone, Deserialize, Serialize)]
pub enum CaseResult {
    Passed(String, Option<u16>),
    Failed(String, Option<u16>, String),
}

impl CaseResult {
    pub(crate) const fn passed(&self) -> bool {
        matches!(self, Self::Passed(_, _))
    }

    pub(crate) fn diagnostic(&self) -> Option<String> {
        match self {
            Self::Failed(_, attempt, error) => {
                Some(attempt.map_or_else(|| error.clone(), |value| format!("attempt {value}: {error}")))
            }
            Self::Passed(_, _) => None,
        }
    }
}

pub async fn run_case(app: Arc<App>, case_index: usize, target: Target) -> Result<Vec<CaseResult>, Error> {
    let case = &app.cases[case_index];
    let timeout = case.soak.as_ref().map_or_else(
        || Duration::from_secs(case.timeout),
        super::scheduler::Plan::total_duration,
    );
    worker::run(&app.name, &case.id, target, timeout).await
}

pub(crate) async fn worker(options: WorkerOptions) -> Result<(), Error> {
    worker::execute(options).await
}

async fn run_case_inner(app: Arc<App>, case_index: usize, target: Target) -> Result<Vec<CaseResult>, Error> {
    let execution = app.execution.container()?;
    if let Some(unwired) = app.cases[case_index].engine_options.unwired() {
        return Err(unwired.into());
    }
    let building = Arc::clone(&app);
    let artifact = tokio::task::spawn_blocking(move || {
        building
            .build(&building.cases[case_index], target)
            .map_err(|error| error.to_string())
    })
    .await??;
    let case = &app.cases[case_index];
    let fixture = TestImage::materialize(&app.image, &target.platform())
        .await
        .map_err(|error| format!("materialize image {} for {}: {error}", app.image, target.name()))?;
    let state = tempfile::tempdir().map_err(|error| format!("create container state directory: {error}"))?;
    let mut config = Config::new(state.path());
    if let Some(cache) = case.engine_options.translation_cache() {
        config = config.translation_cache(cache);
    }
    let containers = hl_container::Containers::builder(config).build().await?;
    let destination = fixture.path().join(case.destination.trim_start_matches('/'));
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| context("create staging directory", parent, &error))?;
    }
    fs::copy(artifact.path(), &destination).map_err(|error| {
        format!(
            "stage {} into {}: {error}",
            artifact.path().display(),
            destination.display()
        )
    })?;
    make_executable(&destination).map_err(|error| context("make executable", &destination, &error))?;
    let results = CaseExecution::new(&app, case, target, fixture.path(), &containers, execution)
        .run()
        .await;
    fixture.release()?;
    Ok(results)
}

/// Names the failing operation and its path, so a bare `os error 2` is never the whole diagnostic.
fn context(operation: &str, path: &std::path::Path, error: &std::io::Error) -> String {
    format!("{operation} {}: {error}", path.display())
}

#[cfg(unix)]
fn make_executable(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[cfg(windows)]
fn make_executable(_path: &std::path::Path) -> std::io::Result<()> {
    // Windows has no executable permission bit. The guest loader consumes the
    // staged Linux image as bytes, so copying it completed this host-side step.
    Ok(())
}

struct CaseExecution<'a> {
    app: &'a App,
    case: &'a super::definition::RuntimeCase,
    target: Target,
    fixture: &'a std::path::Path,
    containers: &'a Containers,
    execution: hl_container::Execution,
}

impl<'a> CaseExecution<'a> {
    fn new(
        app: &'a App,
        case: &'a super::definition::RuntimeCase,
        target: Target,
        fixture: &'a std::path::Path,
        containers: &'a Containers,
        execution: hl_container::Execution,
    ) -> Self {
        if let Some(plan) = &case.soak {
            let resources = plan.resources();
            println!(
                "SOAK {} {} attempts={} duration={}s resources=cpu:{},memory_mib:{},processes:{} (admission only)",
                case.id,
                target.name(),
                plan.repetitions(),
                plan.duration().as_secs(),
                resources.cpu(),
                resources.memory_mib(),
                resources.processes()
            );
        }
        Self {
            app,
            case,
            target,
            fixture,
            containers,
            execution,
        }
    }

    async fn run(&self) -> Vec<CaseResult> {
        let Some(plan) = &self.case.soak else {
            return vec![self.attempt(1, 1, Duration::from_secs(self.case.timeout)).await];
        };
        let end = Instant::now() + plan.total_duration();
        let mut results = Vec::with_capacity(plan.attempts().len());
        for attempt in plan.attempts() {
            let remaining = end.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                results.push(CaseResult::Failed(
                    self.case.id.clone(),
                    Some(attempt.ordinal()),
                    "total soak deadline expired before launch".to_owned(),
                ));
                break;
            }
            results.push(
                self.attempt(attempt.ordinal(), plan.repetitions(), plan.duration().min(remaining))
                    .await,
            );
        }
        results
    }

    async fn attempt(&self, ordinal: u16, repetitions: u16, timeout: Duration) -> CaseResult {
        let attempt = (repetitions > 1).then_some(ordinal);
        let name = format!(
            "testing-{}-{}-{}-{ordinal}",
            self.app.name,
            self.target.name(),
            self.case.id.replace('/', "-")
        );
        let mut process = Process::new(&self.case.destination).args(self.case.arguments.iter().map(String::as_str));
        for entry in &self.case.environment {
            process = process.env_bytes(entry.name().to_vec(), entry.value().to_vec());
        }
        let options = &self.case.engine_options;
        if let Some((uid, gid)) = options.user() {
            process = process.user(uid, gid);
        }
        let mut spec = ContainerSpec::from_directory(self.fixture, process)
            .name(&name)
            .guest(self.target.guest())
            .execution(self.execution)
            .isolation(options.isolation(Isolation {
                sandbox: Sandbox::Disabled,
                ..Isolation::default()
            }))
            .network_mode(options.network_mode())
            .resources(options.resources());
        for mount in options.mounts() {
            spec = spec.mount(mount.clone());
        }
        let outcome = self
            .execute(spec, &name, timeout)
            .await
            .map_err(|error| error.to_string());
        let cleanup = self.containers.remove_force(&name).await;
        match (outcome, cleanup) {
            (Ok(()), Ok(_)) => CaseResult::Passed(self.case.id.clone(), attempt),
            (Err(error), _) => CaseResult::Failed(self.case.id.clone(), attempt, error),
            (Ok(()), Err(error)) => {
                CaseResult::Failed(self.case.id.clone(), attempt, format!("cleanup failed: {error}"))
            }
        }
    }

    async fn execute(&self, spec: ContainerSpec, name: &str, timeout: Duration) -> Result<(), Error> {
        self.containers.create(spec).await?;
        self.containers.start(name).await?;
        let status = self.wait(name, timeout).await?;
        let logs = self.containers.logs(name).await?;
        logs.bounded()?;
        let expected =
            fs::read(&self.case.golden).map_err(|error| context("read golden", &self.case.golden, &error))?;
        if status != ExitStatus::Code(self.case.exit) {
            return Err(format!("exit {status:?}, expected {}", self.case.exit).into());
        }
        if logs.stdout != expected {
            return Err(super::diagnostic::compare("stdout", &logs.stdout, &expected).into());
        }
        if !logs.stderr.is_empty() {
            return Err(format!("unexpected stderr: {}", super::diagnostic::preview(&logs.stderr)).into());
        }
        Ok(())
    }
}

impl CaseExecution<'_> {
    async fn wait(&self, name: &str, timeout: Duration) -> Result<ExitStatus, Error> {
        let waiting = self.containers.wait(name);
        tokio::pin!(waiting);
        let deadline = Instant::now() + timeout;
        loop {
            tokio::select! {
                result = &mut waiting => return Ok(result?),
                () = tokio::time::sleep_until(deadline) => {
                    return Err(format!("timed out after {} milliseconds", timeout.as_millis()).into());
                }
                () = tokio::time::sleep(Duration::from_millis(10)) => {
                    self.containers.logs(name).await?.bounded()?;
                }
            }
        }
    }
}

