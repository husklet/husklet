//! Application composition boundary for host process construction.

use hl_process::{Capture, Command, Outcome};
use std::{ffi::OsStr, io, sync::atomic::AtomicBool, time::Duration};

const PROCESS_CAPTURE_LIMIT: u64 = 64 * 1024;

/// Owns construction of host processes used by the testing application.
pub(crate) struct HostProcess;

impl HostProcess {
    pub(crate) fn standard(program: impl AsRef<OsStr>) -> std::process::Command {
        std::process::Command::new(program)
    }

    pub(crate) fn exact_process_count(name: &str) -> io::Result<u64> {
        let output = Self::standard("pgrep").args(["-cx", name]).output()?;
        decode_process_count(output.status.code(), &output.stdout, &output.stderr)
    }

    pub(crate) fn bounded(program: impl AsRef<OsStr>, arguments: &[String], timeout: Duration) -> io::Result<Outcome> {
        let directory = tempfile::tempdir()?;
        let capture = Capture {
            stdout: directory.path().join("stdout"),
            stderr: directory.path().join("stderr"),
            stdout_limit: PROCESS_CAPTURE_LIMIT,
            stderr_limit: PROCESS_CAPTURE_LIMIT,
        };
        let mut command = Command::new(program);
        command.args(arguments);
        hl_process::run(&command, &capture, timeout, &AtomicBool::new(false))
    }
}

fn decode_process_count(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> io::Result<u64> {
    let count = std::str::from_utf8(stdout)
        .map_err(io::Error::other)?
        .trim()
        .parse::<u64>()
        .map_err(io::Error::other)?;
    match (code, count) {
        (Some(0), count) => Ok(count),
        (Some(1), 0) => Ok(0),
        _ => Err(io::Error::other(format!(
            "pgrep failed with status {}: {}",
            code.map_or_else(|| "signal".to_owned(), |value| value.to_string()),
            String::from_utf8_lossy(stderr).trim()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{HostProcess, decode_process_count};
    use hl_process::Outcome;
    use std::time::{Duration, Instant};

    #[test]
    fn process_count_requires_a_valid_success_or_no_match_result() {
        assert_eq!(decode_process_count(Some(0), b"7\n", b"").unwrap(), 7);
        assert_eq!(decode_process_count(Some(1), b"0\n", b"").unwrap(), 0);
        assert!(decode_process_count(Some(2), b"0\n", b"bad pattern").is_err());
        assert!(decode_process_count(Some(0), b"not-a-count\n", b"").is_err());
        assert!(decode_process_count(None, b"0\n", b"").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn bounded_process_reports_timeout_without_waiting_for_the_guest() {
        let started = Instant::now();
        let outcome = HostProcess::bounded(
            "sh",
            &["-c".to_owned(), "sleep 60 & wait".to_owned()],
            Duration::from_millis(25),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
