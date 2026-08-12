mod artifact;

pub(super) use artifact::{build_artifact, environment, hash_tool, materialize};

use hl_process::{Capture, Command, Outcome};
use std::{fs, sync::atomic::AtomicBool, time::Duration};

#[derive(Debug)]
pub(super) struct ProcessOutput {
    pub(super) status: Option<i32>,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
}

impl ProcessOutput {
    pub(super) fn capture(arguments: &[String], timeout: Duration, limit: usize) -> Result<Self, String> {
        let (program, guest) = arguments.split_first().ok_or("empty nested command")?;
        let output = tempfile::tempdir().map_err(|error| format!("capture directory failed: {error}"))?;
        let capture = Capture {
            stdout: output.path().join("stdout"),
            stderr: output.path().join("stderr"),
            stdout_limit: u64::try_from(limit).map_err(|_| "capture limit exceeds u64")?,
            stderr_limit: u64::try_from(limit).map_err(|_| "capture limit exceeds u64")?,
        };
        let mut command = Command::new(program);
        command.args(guest);
        let outcome = hl_process::run(&command, &capture, timeout, &AtomicBool::new(false))
            .map_err(|error| format!("nested process failed: {error}"))?;
        let status = match outcome {
            Outcome::Exited(status) => status,
            Outcome::Signaled(_) => None,
            Outcome::TimedOut => return Err(format!("timed out after {} seconds", timeout.as_secs())),
            Outcome::Cancelled => return Err("nested process was cancelled".into()),
            Outcome::OutputLimit => return Err(format!("output exceeded {limit} bytes")),
        };
        let stdout = fs::read(&capture.stdout).map_err(|error| format!("stdout capture failed: {error}"))?;
        let stderr = fs::read(&capture.stderr).map_err(|error| format!("stderr capture failed: {error}"))?;
        Ok(Self { status, stdout, stderr })
    }
}
