#[path = "failure_retention.rs"]
mod retention;
#[path = "execution_worker.rs"]
mod worker;

use super::diagnostic::Excerpt as _;
use super::{
    Error,
    definition::App,
    image::{Materialization, TestImage},
    output,
};
use crate::suite::{BoundedCapture as _, Target};
use hl_container::{Config, ContainerSpec, Containers, ExitStatus, Isolation, Process, Sandbox};
use retention::FailureRetention;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path, sync::Arc, time::Duration};
use tokio::time::Instant;

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

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

async fn run_case_inner(
    app: Arc<App>,
    case_index: usize,
    target: Target,
    retention: Option<FailureRetention>,
) -> Result<Vec<CaseResult>, Error> {
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
    let mode = if case.engine_options.mounts().iter().any(|mount| mount.populate) {
        Materialization::Copy
    } else {
        Materialization::from_environment()
    };
    let mut fixture = materialize(&app, case, target, mode).await?;
    let state = tempfile::tempdir().map_err(|error| format!("create container state directory: {error}"))?;

    let mut config = Config::new(state.path());
    if let Some(cache) = case.engine_options.translation_cache() {
        config = config.translation_cache(cache);
    }
    let containers = hl_container::Containers::builder(config)
        .images(fixture.images())
        .build()
        .await?;
    let results = CaseExecution::new(&app, case, target, &containers, execution, retention.as_ref())
        .run(&mut fixture, artifact.path())
        .await;
    fixture.release()?;
    Ok(results)
}

async fn materialize(
    app: &App,
    case: &super::definition::Workload,
    target: Target,
    mode: Materialization,
) -> Result<TestImage, Error> {
    let fixture = match case.rootfs {
        super::definition::Rootfs::Image => TestImage::materialize_with(&app.image, &target.platform(), mode)
            .await
            .map_err(|error| format!("materialize image {} for {}: {error}", app.image, target.name()))?,
        super::definition::Rootfs::Scratch => TestImage::materialize_scratch(&target.platform(), mode)
            .map_err(|error| format!("materialize scratch root for {}: {error}", target.name()))?,
    };
    Ok(fixture)
}

async fn stage(
    fixture: &TestImage,
    case: &super::definition::Workload,
    artifact: &Path,
    target: Target,
) -> Result<(), Error> {
    let root = fixture.path();
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
    provision(root, fixture.lower(), case, target).await
}

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

async fn provision(
    root: &std::path::Path,
    lower: Option<&std::path::Path>,
    case: &super::definition::Workload,
    target: Target,
) -> Result<(), Error> {
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
    for elf in &case.guest_elf {
        let relative = elf.path().trim_start_matches('/');
        let upper = root.join(relative);
        let path = if upper.exists() {
            upper
        } else {
            lower.map_or(upper, |base| base.join(relative))
        };
        super::definition::elf::verify(&path, target, elf.expectation())?;
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
    retention: Option<&'a FailureRetention>,
}

impl<'a> CaseExecution<'a> {
    fn new(
        app: &'a App,
        case: &'a super::definition::Workload,
        target: Target,
        containers: &'a Containers,
        execution: hl_container::Execution,
        retention: Option<&'a FailureRetention>,
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
            retention,
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
            stage(fixture, self.case, artifact, self.target).await?;
            assert_overlay(fixture, self.case)
        }
        .await;
        if let Err(error) = prepared {
            return CaseResult::Failed(self.case.id.clone(), attempt, error.to_string());
        }
        self.attempt(fixture, artifact, ordinal, repetitions, timeout).await
    }

    async fn attempt(
        &self,
        fixture: &TestImage,
        artifact: &Path,
        ordinal: u16,
        repetitions: u16,
        timeout: Duration,
    ) -> CaseResult {
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
            .execution(self.execution)
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
        let mut status = None;
        let outcome = self
            .execute(spec, &name, timeout, &mut status)
            .await
            .map_err(|error| error.to_string());
        let retained = if outcome.is_err() {
            self.retention.and_then(|retention| {
                retention
                    .retain(fixture, artifact, &self.case.id, self.target, ordinal, status)
                    .map_err(|error| eprintln!("failed to retain overlay for {}: {error}", self.case.id))
                    .ok()
            })
        } else {
            None
        };
        let cleanup = tokio::time::timeout(CLEANUP_TIMEOUT, self.containers.remove_force(&name)).await;
        match (outcome, cleanup) {
            (Ok(()), Ok(Ok(_))) => CaseResult::Passed(self.case.id.clone(), attempt),
            (Err(mut error), _) => {
                if let Some(path) = retained {
                    error.push_str(&format!("; retained_overlay={}", path.display()));
                }
                CaseResult::Failed(self.case.id.clone(), attempt, error)
            }
            (Ok(()), Ok(Err(error))) => {
                CaseResult::Failed(self.case.id.clone(), attempt, format!("cleanup failed: {error}"))
            }
            (Ok(()), Err(_)) => CaseResult::Failed(
                self.case.id.clone(),
                attempt,
                cleanup_timeout_diagnostic(CLEANUP_TIMEOUT),
            ),
        }
    }

    async fn execute(
        &self,
        spec: ContainerSpec,
        name: &str,
        timeout: Duration,
        observed: &mut Option<ExitStatus>,
    ) -> Result<(), Error> {
        self.containers.create(spec).await?;
        if let Some((network, endpoint)) = self.case.engine_options.bridge()? {
            let networks = self.containers.networks();
            let created = networks.create(network).await?;
            networks.connect(&created.name, name, endpoint).await?;
        }
        self.containers.start(name).await?;
        let status = if let Some(orchestration) = self.case.orchestration {
            let delay = Duration::from_millis(orchestration.stop_after_ms);
            tokio::time::sleep(delay).await;
            // The manifest timeout bounds the complete attempt, including the pre-stop delay.
            // Validation proves the delay is shorter, so this never silently widens the case.
            self.containers.stop(name, timeout.saturating_sub(delay)).await?
        } else {
            self.wait(name, timeout).await?
        };
        *observed = Some(status);
        let mut logs = self.containers.logs(name).await?;
        logs.bounded()?;
        let mut profile_validation = Ok(());
        if self.execution.diagnostics() {
            let text = std::str::from_utf8(&logs.stderr).map_err(|_| "retained C diagnostics are not UTF-8")?;
            // Preserve a missing profile as a failure for otherwise-correct diagnostic runs, but do not let
            // secondary telemetry loss hide the guest's exit or output failure. Abnormal engine paths can end
            // before the retained dispatcher emits its summary, and the underlying compatibility defect is the
            // actionable diagnostic in that case.
            // An explicitly orchestrated signal ends the engine before its normal dispatcher
            // epilogue. The lifecycle result is the contract for that typed path; all ordinary
            // exits still require the complete profile summary.
            profile_validation = self.case.expected_signal.map_or_else(|| output::validate_profile(text), |_| Ok(()));
            output::forward_profile(text, std::io::stderr().lock())?;
            logs.stderr = text
                .lines()
                .filter(|line| !line.starts_with("[prof] "))
                .flat_map(|line| [line.as_bytes(), b"\n"].concat())
                .collect();
        }
        let expected = if let Some(golden) = &self.case.golden {
            tokio::fs::read(golden)
                .await
                .map_err(|error| context("read golden", golden, &error))?
        } else {
            Vec::new()
        };
        super::outcome::validate(
            status,
            self.case.exit,
            self.case.expected_signal,
            &logs,
            &expected,
            &self.case.stderr,
            profile_validation,
        )
    }
}

fn cleanup_timeout_diagnostic(timeout: Duration) -> String {
    format!("forced cleanup timed out after {} milliseconds", timeout.as_millis())
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
                    let logs = self.containers.logs(name).await?;
                    logs.bounded()?;
                    return Err(format!(
                        "guest exit wait timed out after {} milliseconds; stderr={}; stdout={}",
                        timeout.as_millis(),
                        logs.stderr.preview(),
                        logs.stdout.preview()
                    ).into());
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
    use super::retention::HashedBoundedWriter;
    use super::{CLEANUP_TIMEOUT, FailureRetention, cleanup_timeout_diagnostic, materialize};
    use crate::{runtime::definition::App, suite::Target};
    use hl_container::{ExitStatus, Logs};
    use std::io::Write as _;

    fn missing_profile() -> Result<(), super::Error> {
        Err("retained C profile omitted the crossings/translations summary".into())
    }

    #[test]
    fn cleanup_timeout_names_the_stuck_lifecycle_stage() {
        assert_eq!(
            cleanup_timeout_diagnostic(CLEANUP_TIMEOUT),
            "forced cleanup timed out after 10000 milliseconds"
        );
    }

    #[test]
    fn exit_failure_precedes_missing_profile() {
        let error =
            super::super::outcome::validate(ExitStatus::Code(7), 0, None, &Logs::default(), b"", &[], missing_profile())
                .unwrap_err();
        assert_eq!(error.to_string(), "exit Code(7), expected Code(0)");
    }

    #[test]
    fn stdout_failure_precedes_missing_profile() {
        let logs = Logs {
            stdout: b"wrong".to_vec(),
            stderr: Vec::new(),
        };
        let error = super::super::outcome::validate(ExitStatus::Code(0), 0, None, &logs, b"right", &[], missing_profile())
            .unwrap_err();
        assert!(error.to_string().starts_with("stdout differs:"), "{error}");
    }

    #[test]
    fn otherwise_valid_output_still_requires_profile() {
        let error =
            super::super::outcome::validate(ExitStatus::Code(0), 0, None, &Logs::default(), b"", &[], missing_profile())
                .unwrap_err();
        assert!(error.to_string().contains("crossings/translations"), "{error}");
    }

    #[tokio::test]
    async fn scratch_declaration_bypasses_the_document_image_and_yields_an_empty_root() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("probe.c"), "probe").unwrap();
        std::fs::create_dir(directory.path().join("golden")).unwrap();
        std::fs::write(directory.path().join("golden/probe.out"), []).unwrap();
        let definition = directory.path().join("test.yaml");
        std::fs::write(
            &definition,
            "targets: [arm64]\nimage: ':not-an-image'\nexecution: {}\nbuild:\n  compiler: { arm64: cc, amd64: cc }\n  flags: []\ncases:\n  - id: runtime/scratch-dispatch\n    status: active\n    compat: { class: compatibility }\n    rootfs: scratch\n    build: { source: probe.c, output: probe, flags: [] }\n    artifact: { destination: /probe }\n    run: []\n    expect: { exit: 0, stdout: golden/probe.out }\n",
        )
        .unwrap();
        let app = App::load(directory.path(), &definition).unwrap();

        let fixture = materialize(&app, &app.cases[0], Target::Arm64, super::Materialization::Overlay)
            .await
            .unwrap();
        assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(fixture.lower().unwrap()).unwrap().count(), 0);
        fixture.release().unwrap();
    }

    #[test]
    fn retained_overlay_writer_refuses_the_first_byte_past_its_bound() {
        let mut writer = HashedBoundedWriter::new(Vec::new(), 3);
        writer.write_all(b"abc").unwrap();
        let error = writer.write_all(b"d").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::FileTooLarge);
        let (bytes, digest) = writer.finish();
        assert_eq!(bytes, 3);
        assert_eq!(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn failed_overlay_archive_and_signal_manifest_are_correlated_before_release() {
        let platform = Target::Arm64.platform();
        let fixture = super::TestImage::materialize_scratch(&platform, super::Materialization::Overlay).unwrap();
        std::fs::write(fixture.path().join("layout-dependent.pyc"), b"failed-layout").unwrap();
        let artifact_dir = tempfile::tempdir().unwrap();
        let artifact = artifact_dir.path().join("python");
        std::fs::write(&artifact, b"exact-python-binary").unwrap();
        let output = tempfile::tempdir().unwrap();
        let retention = FailureRetention::new(output.path().to_owned(), "a".repeat(64));
        let status = ExitStatus::Fault {
            status: 11,
            detail: 0x1234,
            reason: hl_container::FaultCause::Memory,
        };
        let retained = retention
            .retain(
                &fixture,
                &artifact,
                "runtime/python/layout",
                Target::Arm64,
                7,
                Some(status),
            )
            .unwrap();

        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(retained.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["attempt"], 7);
        assert_eq!(manifest["status"]["kind"], "fault");
        assert_eq!(manifest["status"]["value"]["detail"], 0x1234);
        assert_eq!(
            manifest["artifact_sha256"],
            "509c1bf3e9e87736aaf9c75daf41a40532c0402ec32e4de571aff181a1bfae63"
        );
        let archive = std::fs::File::open(retained.join("upper.tar")).unwrap();
        let mut archive = tar::Archive::new(archive);
        let mut entries = archive.entries().unwrap();
        assert!(entries.any(|entry| entry.unwrap().path().unwrap().ends_with("layout-dependent.pyc")));
        fixture.release().unwrap();
        assert!(
            retained.join("upper.tar").is_file(),
            "release must not consume retained evidence"
        );
    }
}
