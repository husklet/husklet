//! Application composition boundary for host process construction.

use hl_process::{Capture, Command, Outcome};
use std::{ffi::OsStr, io, sync::atomic::AtomicBool, time::Duration};

const PROCESS_CAPTURE_LIMIT: u64 = 64 * 1024;

/// Owns construction of host processes used by the testing application.
pub(crate) struct HostProcess;

pub(crate) struct ProcessCapture {
    pub outcome: Outcome,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl HostProcess {
    pub(crate) fn standard(program: impl AsRef<OsStr>) -> std::process::Command {
        std::process::Command::new(program)
    }

    pub(crate) fn exact_process_count(name: &str) -> io::Result<u64> {
        let output = Self::standard("pgrep").args(["-cx", name]).output()?;
        decode_process_count(output.status.code(), &output.stdout, &output.stderr)
    }

    pub(crate) fn bounded(program: impl AsRef<OsStr>, arguments: &[String], timeout: Duration) -> io::Result<Outcome> {
        Ok(Self::bounded_capture(program, arguments, timeout)?.outcome)
    }

    pub(crate) fn bounded_capture(
        program: impl AsRef<OsStr>,
        arguments: &[String],
        timeout: Duration,
    ) -> io::Result<ProcessCapture> {
        let directory = tempfile::tempdir()?;
        let capture = Capture {
            stdout: directory.path().join("stdout"),
            stderr: directory.path().join("stderr"),
            stdout_limit: PROCESS_CAPTURE_LIMIT,
            stderr_limit: PROCESS_CAPTURE_LIMIT,
        };
        let mut command = Command::new(program);
        command.args(arguments);
        let outcome = hl_process::run(&command, &capture, timeout, &AtomicBool::new(false))?;
        Ok(ProcessCapture {
            outcome,
            stdout: std::fs::read(capture.stdout)?,
            stderr: std::fs::read(capture.stderr)?,
        })
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
    use fs2::FileExt as _;
    use hl_process::Outcome;
    use std::{
        fs,
        time::{Duration, Instant},
    };

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

    #[cfg(target_os = "linux")]
    #[test]
    fn benchmark_timeout_contains_reexec_session_and_holds_outer_lock_until_settled() {
        let directory = tempfile::tempdir().unwrap();
        let lock = directory.path().join("box.lock");
        let ready = directory.path().join("descendant.ready");
        let settled = directory.path().join("runner.settled");
        let executable = std::env::current_exe().unwrap();
        let mut runner = std::process::Command::new(&executable)
            .args(["--exact", "platform::tests::benchmark_containment_runner", "--ignored"])
            .env("HL_PROCESS_BENCH_LOCK", &lock)
            .env("HL_PROCESS_BENCH_READY", &ready)
            .env("HL_PROCESS_BENCH_SETTLED", &settled)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists(), "detached re-exec descendant did not become ready");

        let contender = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock)
            .unwrap();
        while !settled.exists() {
            assert!(
                contender.try_lock_exclusive().is_err(),
                "box lock released before descendants settled"
            );
            assert!(
                runner.try_wait().unwrap().is_none(),
                "runner exited before publishing settlement"
            );
            assert!(Instant::now() < deadline, "containment runner did not settle");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(runner.wait().unwrap().success());
        contender.try_lock_exclusive().unwrap();
        contender.unlock().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "executed as the lock-owning benchmark containment runner"]
    fn benchmark_containment_runner() {
        let lock = std::env::var_os("HL_PROCESS_BENCH_LOCK").unwrap();
        let ready = std::env::var_os("HL_PROCESS_BENCH_READY").unwrap();
        let settled = std::env::var_os("HL_PROCESS_BENCH_SETTLED").unwrap();
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock)
            .unwrap();
        lock.lock_exclusive().unwrap();
        let executable = std::env::current_exe().unwrap();
        let script = "exec >/dev/null 2>&1; trap '' TERM; exec \"$1\" --exact platform::tests::benchmark_containment_descendant --ignored";
        let arguments = vec![
            "--wait".to_owned(),
            "sh".to_owned(),
            "-c".to_owned(),
            script.to_owned(),
            "fixture".to_owned(),
            executable.to_string_lossy().into_owned(),
        ];
        let outcome = HostProcess::bounded_capture("setsid", &arguments, Duration::from_millis(250))
            .unwrap()
            .outcome;
        assert_eq!(outcome, Outcome::TimedOut);
        let identity = fs::read_to_string(&ready).unwrap();
        let (pid, started) = identity.trim().split_once(' ').unwrap();
        let stat = fs::read_to_string(format!("/proc/{pid}/stat"));
        if let Ok(stat) = stat {
            let live_started = stat
                .rsplit_once(") ")
                .unwrap()
                .1
                .split_ascii_whitespace()
                .nth(19)
                .unwrap();
            assert_ne!(
                live_started, started,
                "exact detached descendant identity remained live"
            );
        }
        fs::write(settled, b"settled\n").unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "executed as a detached re-exec descendant"]
    fn benchmark_containment_descendant() {
        let ready = std::env::var_os("HL_PROCESS_BENCH_READY").unwrap();
        let pid = std::process::id();
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let started = stat
            .rsplit_once(") ")
            .unwrap()
            .1
            .split_ascii_whitespace()
            .nth(19)
            .unwrap();
        fs::write(ready, format!("{pid} {started}\n")).unwrap();
        std::thread::sleep(Duration::from_secs(2));
    }
}
