#[path = "execution_worker.rs"]
mod worker;

use super::diagnostic::Excerpt as _;
use super::{
    Error,
    definition::App,
    image::{Materialization, TestImage},
};
use crate::suite::{BoundedCapture as _, Target};
use hl_container::{Config, ContainerSpec, Containers, ExitStatus, Isolation, Process, Sandbox};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path, sync::Arc, time::Duration};
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

/// A case outcome plus the engine counters observed while it ran.
///
/// The counters were stderr-only and vanished with the run, so no sweep could be audited
/// afterwards for which cases actually translated their own body.
pub struct Report {
    pub results: Vec<CaseResult>,
    /// One `native counter=value ...` line, empty when the app does not emit diagnostics.
    pub counters: String,
}

pub async fn run_case(app: Arc<App>, case_index: usize, target: Target) -> Result<Report, Error> {
    let case = &app.cases[case_index];
    worker::run(&app.name, &case.id, target, case.declared_timeout(), &case.diagnostics).await
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
    // A mount that must be populated needs a materialized tree, exactly as the
    // product's `create_image` falls back from overlay to a full copy.
    let mode = if case.engine_options.mounts().iter().any(|mount| mount.populate) {
        Materialization::Copy
    } else {
        Materialization::from_environment()
    };
    let mut fixture = TestImage::materialize_with(&app.image, &target.platform(), mode)
        .await
        .map_err(|error| format!("materialize image {} for {}: {error}", app.image, target.name()))?;
    let state = tempfile::tempdir().map_err(|error| format!("create container state directory: {error}"))?;
    let mut config = Config::new(state.path());
    if let Some(cache) = case.engine_options.translation_cache() {
        config = config.translation_cache(cache);
    }
    let containers = hl_container::Containers::builder(config)
        .images(fixture.images())
        .build()
        .await?;
    let results = CaseExecution::new(&app, case, target, &containers, execution)
        .run(&mut fixture, artifact.path())
        .await;
    fixture.release()?;
    Ok(results)
}

/// Stages the case artifact into the writable root: the overlay upper, or the copied tree.
async fn stage(root: &Path, case: &super::definition::Workload, artifact: &Path, target: Target) -> Result<(), Error> {
    let destination = root.join(case.destination.trim_start_matches('/'));
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| context("create staging directory", parent, &error))?;
    }
    tokio::fs::copy(artifact, &destination)
        .await
        .map_err(|error| format!("stage {} into {}: {error}", artifact.display(), destination.display()))?;
    make_executable(&destination).map_err(|error| context("make executable", &destination, &error))?;
    provision(root, case, target).await
}

/// Proves the run really took the product's overlay path rather than a flat copy.
///
/// The staged program must exist in the upper and nowhere in the lower, and the
/// lower must still carry image content the upper does not: a resolution that
/// launches the program at all has crossed both layers.
fn assert_overlay(fixture: &TestImage, case: &super::definition::Workload) -> Result<(), Error> {
    let Some(lower) = fixture.lower() else {
        return Ok(());
    };
    let relative = case.destination.trim_start_matches('/');
    let upper = fixture.path().join(relative);
    if !upper.exists() {
        return Err(format!(
            "overlay proof: {} is not in the upper {}",
            relative,
            fixture.path().display()
        )
        .into());
    }
    if lower.join(relative).exists() {
        return Err(format!("overlay proof: {relative} is already in the lower {}", lower.display()).into());
    }
    if fixture.reference().overlay().is_none() {
        return Err("overlay proof: rootfs reference carries no lower/upper split"
            .to_owned()
            .into());
    }
    Ok(())
}

/// Stages the guest-side state a case declares, so a fixture never has to depend on the image alone.
async fn provision(root: &std::path::Path, case: &super::definition::Workload, target: Target) -> Result<(), Error> {
    // A dynamically linked case needs its PT_INTERP loader and shared libraries, which the base
    // image's libc does not supply; they come from the same cross toolchain that built the binary.
    for library in &case.guest_libraries {
        let (host, path) = (
            library.host(target),
            root.join(library.guest(target).trim_start_matches('/')),
        );
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| context("create guest library directory", parent, &error))?;
        }
        tokio::fs::copy(host, &path)
            .await
            .map_err(|error| format!("stage guest library {host} into {}: {error}", path.display()))?;
        make_executable(&path).map_err(|error| context("make guest library executable", &path, &error))?;
    }
    for file in &case.guest_files {
        let path = root.join(file.path().trim_start_matches('/'));
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| context("create guest file directory", parent, &error))?;
        }
        tokio::fs::write(&path, file.contents())
            .await
            .map_err(|error| context("write guest file", &path, &error))?;
        set_private(&path).map_err(|error| context("set guest file mode", &path, &error))?;
    }
    if let Some(cwd) = &case.working_directory {
        let path = root.join(cwd.trim_start_matches('/'));
        tokio::fs::create_dir_all(&path)
            .await
            .map_err(|error| context("create guest working directory", &path, &error))?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_private(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(windows)]
fn set_private(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
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
    case: &'a super::definition::Workload,
    target: Target,
    containers: &'a Containers,
    execution: hl_container::Execution,
}

impl<'a> CaseExecution<'a> {
    fn new(
        app: &'a App,
        case: &'a super::definition::Workload,
        target: Target,
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
            containers,
            execution,
        }
    }

    async fn run(&self, fixture: &mut TestImage, artifact: &Path) -> Vec<CaseResult> {
        let Some(plan) = &self.case.soak else {
            let timeout = Duration::from_secs(self.case.timeout);
            return vec![self.staged_attempt(fixture, artifact, 1, 1, timeout, false).await];
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
                self.staged_attempt(
                    fixture,
                    artifact,
                    attempt.ordinal(),
                    plan.repetitions(),
                    plan.duration().min(remaining),
                    attempt.ordinal() > 1,
                )
                .await,
            );
        }
        results
    }

    /// Container removal consumes an image-backed rootfs, so every attempt after
    /// the first gets a fresh writable root and is staged into it again.
    async fn staged_attempt(
        &self,
        fixture: &mut TestImage,
        artifact: &Path,
        ordinal: u16,
        repetitions: u16,
        timeout: Duration,
        refork: bool,
    ) -> CaseResult {
        let attempt = (repetitions > 1).then_some(ordinal);
        let prepared = async {
            if refork {
                fixture.refork()?;
            }
            stage(fixture.path(), self.case, artifact, self.target).await?;
            assert_overlay(fixture, self.case)
        }
        .await;
        if let Err(error) = prepared {
            return CaseResult::Failed(self.case.id.clone(), attempt, error.to_string());
        }
        self.attempt(fixture, ordinal, repetitions, timeout).await
    }

    async fn attempt(&self, fixture: &TestImage, ordinal: u16, repetitions: u16, timeout: Duration) -> CaseResult {
        let attempt = (repetitions > 1).then_some(ordinal);
        let name = format!(
            "testing-{}-{}-{}-{ordinal}",
            self.app.name,
            self.target.name(),
            self.case.id.replace('/', "-")
        );
        let mut process = Process::new(&self.case.destination).args(self.case.arguments.iter().map(String::as_str));
        if let Some(cwd) = &self.case.working_directory {
            process = process.working_dir(cwd);
        }
        for entry in &self.case.environment {
            process = process.env_bytes(entry.name().to_vec(), entry.value().to_vec());
        }
        let options = &self.case.engine_options;
        if let Some((uid, gid)) = options.user() {
            process = process.user(uid, gid);
        }
        let mut spec = ContainerSpec::new(fixture.reference().clone(), process)
            .name(&name)
            .guest(self.target.guest())
            .execution(options.execution(self.execution))
            .isolation(options.isolation(Isolation {
                sandbox: Sandbox::Disabled,
                ..Isolation::default()
            }))
            .network_mode(options.network_mode())
            .resources(options.resources());
        if let Some(hostname) = options.hostname() {
            spec = spec.hostname(hostname);
        }
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
        if let Some((network, endpoint)) = self.case.engine_options.bridge()? {
            let networks = self.containers.networks();
            let created = networks.create(network).await?;
            networks.connect(&created.name, name, endpoint).await?;
        }
        self.containers.start(name).await?;
        let status = self.wait(name, timeout).await?;
        let mut logs = self.containers.logs(name).await?;
        logs.bounded()?;
        if matches!(self.execution, hl_container::Execution::RetainedCDiagnostics) {
            let text = std::str::from_utf8(&logs.stderr).map_err(|_| "retained C diagnostics are not UTF-8")?;
            let profile = text
                .lines()
                .find(|line| line.starts_with("[prof] "))
                .ok_or("retained C profile line was not emitted")?;
            let misses = profile
                .split_whitespace()
                .find_map(|field| field.strip_prefix("ibtc_miss="))
                .ok_or("retained C profile omitted ibtc_miss")?
                .parse::<u64>()
                .map_err(|_| "retained C ibtc_miss is not an integer")?;
            if misses > 100_000 {
                return Err(format!("retained C warm IBTC misses {misses} exceed 100000").into());
            }
            logs.stderr = text
                .lines()
                .filter(|line| !line.starts_with("[prof] "))
                .flat_map(|line| [line.as_bytes(), b"\n"].concat())
                .collect();
        }
        let expected = tokio::fs::read(&self.case.golden)
            .await
            .map_err(|error| context("read golden", &self.case.golden, &error))?;
        if status != ExitStatus::Code(self.case.exit) {
            return Err(format!("exit {status:?}, expected {}", self.case.exit).into());
        }
        if logs.stdout != expected {
            return Err(super::diagnostic::compare("stdout", &logs.stdout, &expected).into());
        }
        if let Some(violation) = stderr_violation(&self.case.stderr, &logs.stderr) {
            return Err(violation.into());
        }
        Ok(())
    }
}

/// Declared stderr patterns are an assertion, not an allowance: every emitted line must match a
/// declared pattern, and every declared pattern must match a line.
fn stderr_violation(patterns: &[String], stderr: &[u8]) -> Option<String> {
    if patterns.is_empty() {
        return (!stderr.is_empty()).then(|| format!("unexpected stderr: {}", stderr.preview()));
    }
    let Ok(text) = std::str::from_utf8(stderr) else {
        return Some(format!("stderr is not UTF-8: {}", stderr.preview()));
    };
    let lines = text.lines().collect::<Vec<_>>();
    if let Some(line) = lines
        .iter()
        .find(|line| !patterns.iter().any(|pattern| glob(pattern, line)))
    {
        return Some(format!("undeclared stderr line: {line:?}"));
    }
    patterns
        .iter()
        .find(|pattern| !lines.iter().any(|line| glob(pattern, line)))
        .map(|pattern| format!("expected stderr pattern never appeared: {pattern:?}"))
}

/// `*` matches any run of characters; every other character is literal and the match is anchored.
fn glob(pattern: &str, text: &str) -> bool {
    let Some((head, rest)) = pattern.split_once('*') else {
        return pattern == text;
    };
    let Some(mut tail) = text.strip_prefix(head) else {
        return false;
    };
    loop {
        if glob(rest, tail) {
            return true;
        }
        if tail.is_empty() {
            return false;
        }
        let mut rest_of_tail = tail.chars();
        rest_of_tail.next();
        tail = rest_of_tail.as_str();
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

#[cfg(test)]
mod stderr_tests {
    use super::{glob, stderr_violation};

    #[test]
    fn an_undeclared_stderr_line_still_fails_the_case() {
        let patterns = vec!["fdrss base=*KB fin=*KB grew=*KB thresh=122880KB".to_owned()];
        assert!(stderr_violation(&patterns, b"fdrss base=1KB fin=1KB grew=0KB thresh=122880KB\n").is_none());
        let noisy = b"fdrss base=1KB fin=1KB grew=0KB thresh=122880KB\nhl: internal fault\n";
        assert!(
            stderr_violation(&patterns, noisy)
                .unwrap()
                .contains("undeclared stderr line")
        );
    }

    #[test]
    fn a_pattern_that_never_appears_fails_rather_than_passing_silently() {
        let patterns = vec!["A both".to_owned(), "Z done".to_owned()];
        assert!(
            stderr_violation(&patterns, b"A both\n")
                .unwrap()
                .contains("never appeared")
        );
    }

    #[test]
    fn no_declared_pattern_keeps_the_empty_stderr_default() {
        assert!(stderr_violation(&[], b"").is_none());
        assert!(stderr_violation(&[], b"anything").is_some());
    }

    #[test]
    fn wildcards_are_anchored_and_literal_elsewhere() {
        assert!(glob("a*c", "abbbc"));
        assert!(glob("a*c", "ac"));
        assert!(!glob("a*c", "abbbcd"));
        assert!(!glob("abc", "abcd"));
        assert!(glob("[cache-reuse] kind=*", "[cache-reuse] kind=fork"));
    }
}
