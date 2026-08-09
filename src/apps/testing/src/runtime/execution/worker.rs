use super::{CaseResult, Report};
use crate::runtime::definition::diagnostics::{self, Assertion};
use crate::runtime::diagnostic::Excerpt as _;
use crate::runtime::{self, workspace};
use crate::suite::{Error, Target};
use clap::Args;
use hl_process::{Capture, Outcome as ProcessOutcome};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const CAPTURE_LIMIT: u64 = 1024 * 1024;
/// Engine diagnostics are forwarded and compared, but every concurrent worker holds one in
/// memory, so the bound is only loose enough for the loudest observed case.
const DIAGNOSTIC_CAPTURE_LIMIT: u64 = 8 * 1024 * 1024;
const RESULT_LIMIT: u64 = 1024 * 1024;
const SETUP_ALLOWANCE: Duration = Duration::from_secs(30);
/// Later than the supervisor's own bound, so a supervised row still reports through its
/// supervisor and only an unsupervised one is ended here.
const BACKSTOP_ALLOWANCE: Duration = Duration::from_secs(60);
const BACKSTOP_EXIT: i32 = 70;

#[derive(Args)]
pub(crate) struct Options {
    #[arg(long)]
    app: String,
    #[arg(long)]
    case: String,
    #[arg(long, value_enum)]
    target: Target,
    #[arg(long)]
    result: PathBuf,
    #[arg(long, hide = true)]
    token: String,
}

#[derive(Deserialize, Serialize)]
struct Outcome {
    token: String,
    result: Result<Vec<CaseResult>, String>,
}

pub(crate) async fn execute(options: Options) -> Result<(), Error> {
    validate_token(&options.token)?;
    let work = runtime::worker_work(options.app, options.case, options.target)?;
    // A worker outlives its supervisor whenever the sweep is killed, and the deadline inside
    // `CaseExecution::wait` covers neither the build, the image materialization, nor a container
    // removal whose guest refuses to stop. This bound owns the whole worker and needs nobody alive.
    backstop(super::declared_timeout(&work.app.cases[work.case_index]).saturating_add(BACKSTOP_ALLOWANCE))?;
    let result = super::run_case_inner(work.app, work.case_index, work.target)
        .await
        .map_err(|error| error.to_string());
    let text = serde_yaml::to_string(&Outcome {
        token: options.token,
        result: result.clone(),
    })?;
    if text.len() as u64 > RESULT_LIMIT {
        return Err("runtime worker result exceeded its byte bound".into());
    }
    write_result(&options.result, text.as_bytes())?;
    result.map(|_| ()).map_err(Into::into)
}

pub(super) async fn run(
    app: &str,
    case: &str,
    target: Target,
    timeout: Duration,
    assertions: &[Assertion],
) -> Result<Report, Error> {
    let interrupts = Interrupts::new()?;
    let cancellation = Arc::new(AtomicBool::new(false));
    let guard = Cancellation(Arc::clone(&cancellation));
    let app = app.to_owned();
    let case = case.to_owned();
    let assertions = assertions.to_vec();
    let task = tokio::task::spawn_blocking(move || supervise(&app, &case, target, timeout, &cancellation, &assertions));
    let result = interrupted(task, &guard.0, interrupts).await;
    drop(guard);
    result?.map_err(Into::into)
}

/// Ends this process once `bound` elapses, whatever it is doing and whoever is still watching.
fn backstop(bound: Duration) -> Result<(), Error> {
    std::thread::Builder::new()
        .name("runtime-worker-backstop".to_owned())
        .spawn(move || {
            std::thread::sleep(bound);
            let _ = std::io::stderr()
                .lock()
                .write_all(format!("runtime worker exceeded its own {} second bound\n", bound.as_secs()).as_bytes());
            std::process::exit(BACKSTOP_EXIT);
        })
        .map_err(|error| format!("arm runtime worker backstop: {error}"))?;
    Ok(())
}

struct Cancellation(Arc<AtomicBool>);

impl Drop for Cancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

async fn interrupted(
    mut task: tokio::task::JoinHandle<Result<Report, String>>,
    cancellation: &AtomicBool,
    mut interrupts: Interrupts,
) -> Result<Result<Report, String>, String> {
    #[cfg(unix)]
    {
        tokio::select! {
            result = &mut task => joined(result),
            received = interrupts.interrupt.recv() => interrupt_result(received, task, cancellation, "interrupt").await,
            received = interrupts.terminate.recv() => interrupt_result(received, task, cancellation, "termination").await,
            received = interrupts.hangup.recv() => interrupt_result(received, task, cancellation, "hangup").await,
        }
    }
    #[cfg(windows)]
    {
        tokio::select! {
            result = &mut task => joined(result),
            received = interrupts.ctrl_c.recv() => interrupt_result(received, task, cancellation, "control-c").await,
        }
    }
}

async fn interrupt_result(
    received: Option<()>,
    task: tokio::task::JoinHandle<Result<Report, String>>,
    cancellation: &AtomicBool,
    name: &str,
) -> Result<Result<Report, String>, String> {
    cancellation.store(true, Ordering::Release);
    let result = joined(task.await);
    if received.is_none() {
        let _ = result;
        Err(format!("runtime worker {name} listener closed"))
    } else {
        result
    }
}

#[cfg(unix)]
struct Interrupts {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Interrupts {
    fn new() -> Result<Self, String> {
        Ok(Self {
            interrupt: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .map_err(|error| format!("install runtime worker interrupt listener: {error}"))?,
            terminate: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|error| format!("install runtime worker termination listener: {error}"))?,
            hangup: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
                .map_err(|error| format!("install runtime worker hangup listener: {error}"))?,
        })
    }
}

#[cfg(windows)]
struct Interrupts {
    ctrl_c: tokio::signal::windows::CtrlC,
}

#[cfg(windows)]
impl Interrupts {
    fn new() -> Result<Self, String> {
        Ok(Self {
            ctrl_c: tokio::signal::windows::ctrl_c()
                .map_err(|error| format!("install runtime worker control-c listener: {error}"))?,
        })
    }
}

fn joined(result: Result<Result<Report, String>, tokio::task::JoinError>) -> Result<Result<Report, String>, String> {
    result.map_err(|error| format!("runtime worker supervision task failed: {error}"))
}

fn supervise(
    app: &str,
    case: &str,
    target: Target,
    timeout: Duration,
    cancelled: &AtomicBool,
    assertions: &[Assertion],
) -> Result<Report, String> {
    let root = workspace().map_err(|error| error.to_string())?;
    let workers = root.join("target/testing/runtime/workers");
    fs::create_dir_all(&workers).map_err(|error| format!("create worker directory: {error}"))?;
    let directory = tempfile::Builder::new()
        .prefix("row-")
        .tempdir_in(&workers)
        .map_err(|error| format!("create worker workspace: {error}"))?;
    let result = directory.path().join("outcome.yaml");
    let token = token()?;
    let executable = crate::runtime::profile::runner().map_err(|error| error.to_string())?;
    let mut command = hl_process::Command::new(executable);
    command.args([
        "runtime-worker",
        "--app",
        app,
        "--case",
        case,
        "--target",
        target.name(),
        "--result",
    ]);
    command.arg(&result).arg("--token").arg(&token);
    let capture = Capture {
        stdout: directory.path().join("stdout"),
        stderr: directory.path().join("stderr"),
        stdout_limit: CAPTURE_LIMIT,
        stderr_limit: DIAGNOSTIC_CAPTURE_LIMIT,
    };
    let outcome = hl_process::run(&command, &capture, timeout.saturating_add(SETUP_ALLOWANCE), cancelled)
        .map_err(|error| format!("supervise runtime worker: {error}"))?;
    let stdout = read_capture(&capture.stdout, CAPTURE_LIMIT)?;
    let stderr = read_capture(&capture.stderr, DIAGNOSTIC_CAPTURE_LIMIT)?;
    match outcome {
        ProcessOutcome::Exited(Some(0 | 1)) => {}
        ProcessOutcome::Exited(code) => {
            return Err(format!(
                "runtime worker exited with {code:?}; stderr={}; stdout={}",
                stderr.preview(),
                stdout.preview()
            ));
        }
        ProcessOutcome::Signaled(signal) => {
            return Err(format!(
                "runtime worker terminated by signal {signal}; stderr={}; stdout={}",
                stderr.preview(),
                stdout.preview()
            ));
        }
        ProcessOutcome::TimedOut => {
            return Err(format!(
                "runtime worker timed out after {} milliseconds; stderr={}; stdout={}",
                timeout.as_millis(),
                stderr.preview(),
                stdout.preview()
            ));
        }
        ProcessOutcome::Cancelled => {
            return Err(format!(
                "runtime worker was interrupted; stderr={}; stdout={}",
                stderr.preview(),
                stdout.preview()
            ));
        }
        ProcessOutcome::OutputLimit => {
            return Err(format!(
                "runtime worker output exceeded its bound ({CAPTURE_LIMIT} stdout, \
                 {DIAGNOSTIC_CAPTURE_LIMIT} stderr); stderr={}; stdout={}",
                stderr.preview(),
                stdout.preview()
            ));
        }
    }
    std::io::stderr()
        .lock()
        .write_all(&stderr)
        .map_err(|error| format!("forward runtime worker diagnostics: {error}"))?;
    let bytes = read_capture(&result, RESULT_LIMIT)?;
    let decoded: Outcome =
        serde_yaml::from_slice(&bytes).map_err(|error| format!("decode runtime worker result: {error}"))?;
    if decoded.token != token {
        return Err("runtime worker result correlation token mismatched".to_owned());
    }
    Ok(Report {
        results: judged(decoded.result?, case, assertions, &stderr),
        counters: diagnostics::digest(&stderr),
    })
}

/// Counters accumulate over the whole worker run, so an assertion judges the aggregate and every
/// attempt of a soak case carries the same verdict.
fn judged(results: Vec<CaseResult>, case: &str, assertions: &[Assertion], stderr: &[u8]) -> Vec<CaseResult> {
    let Some(violation) = diagnostics::violation(assertions, stderr) else {
        return results;
    };
    if results.is_empty() {
        return vec![CaseResult::Failed(case.to_owned(), None, violation)];
    }
    results
        .into_iter()
        .map(|result| match result {
            CaseResult::Passed(id, attempt) => CaseResult::Failed(id, attempt, violation.clone()),
            failed @ CaseResult::Failed(_, _, _) => failed,
        })
        .collect()
}

fn token() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "generate runtime worker correlation token".to_owned())?;
    Ok(bytes.iter().map(|value| format!("{value:02x}")).collect())
}

fn validate_token(token: &str) -> Result<(), Error> {
    if token.len() == 64 && token.bytes().all(|value| value.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("runtime worker token is malformed".into())
    }
}

fn read_capture(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if metadata.len() > limit {
        return Err(format!("{} exceeded its byte bound", path.display()));
    }
    fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn write_result(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_data()
}

#[cfg(test)]
mod tests {
    use super::{Outcome, token, validate_token, write_result};
    use std::fs;

    #[test]
    fn tokens_are_typed_random_and_distinct() {
        let first = token().unwrap();
        let second = token().unwrap();
        validate_token(&first).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn path_is_not_a_correlation_token() {
        assert!(super::validate_token("/tmp/spoof").is_err());
    }

    #[test]
    fn existing_result_is_never_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("result");
        fs::write(&path, b"owned").unwrap();
        let error = write_result(&path, b"replacement").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(path).unwrap(), b"owned");
    }

    #[test]
    fn wrong_token_is_detectable() {
        let encoded = serde_yaml::to_string(&Outcome {
            token: "0".repeat(64),
            result: Ok(Vec::new()),
        })
        .unwrap();
        let decoded: Outcome = serde_yaml::from_str(&encoded).unwrap();
        assert_ne!(decoded.token, "1".repeat(64));
    }
}
