use super::{Capture, Command as OwnedCommand, Outcome};
use std::fs;
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(10);
const TERM_GRACE: Duration = Duration::from_millis(500);

pub(super) fn run(
    command: &OwnedCommand,
    capture: &Capture,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> std::io::Result<Outcome> {
    let mut command = command.standard();
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut owned = OwnedChild::new(command.spawn()?);
    let stdout = Drain::spawn(
        owned
            .child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("stdout was not piped"))?,
        capture.stdout_limit,
    );
    let stderr = Drain::spawn(
        owned
            .child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("stderr was not piped"))?,
        capture.stderr_limit,
    );
    let started = Instant::now();
    let outcome = loop {
        if stdout.exceeded() || stderr.exceeded() {
            owned.terminate()?;
            break Outcome::OutputLimit;
        }
        if cancelled.load(Ordering::Acquire) {
            owned.terminate()?;
            break Outcome::Cancelled;
        }
        if started.elapsed() >= timeout {
            owned.terminate()?;
            break Outcome::TimedOut;
        }
        match owned.try_wait() {
            Ok(Some(status)) => {
                owned.quiesce()?;
                break status
                    .signal()
                    .map_or_else(|| Outcome::Exited(status.code()), Outcome::Signaled);
            }
            Ok(None) => thread::sleep(POLL),
            Err(error) => {
                let cleanup = owned.terminate();
                return cleanup.and(Err(error));
            }
        }
    };
    let stdout = stdout.finish()?;
    let stderr = stderr.finish()?;
    let exceeded = stdout.exceeded || stderr.exceeded;
    fs::write(&capture.stdout, stdout.bytes)?;
    fs::write(&capture.stderr, stderr.bytes)?;
    Ok(if exceeded { Outcome::OutputLimit } else { outcome })
}

struct Drained {
    bytes: Vec<u8>,
    exceeded: bool,
}

struct Drain {
    count: Arc<AtomicU64>,
    limit: u64,
    thread: thread::JoinHandle<std::io::Result<Vec<u8>>>,
}

impl Drain {
    fn spawn(mut source: impl Read + Send + 'static, limit: u64) -> Self {
        let count = Arc::new(AtomicU64::new(0));
        let observed = Arc::clone(&count);
        let thread = thread::spawn(move || {
            let capacity = usize::try_from(limit.min(1024 * 1024)).unwrap_or(1024 * 1024);
            let mut retained = Vec::with_capacity(capacity);
            let mut buffer = [0_u8; 16 * 1024];
            loop {
                let size = source.read(&mut buffer)?;
                if size == 0 {
                    break;
                }
                observed.fetch_add(size as u64, Ordering::Release);
                let available = usize::try_from(limit.saturating_sub(retained.len() as u64)).unwrap_or(usize::MAX);
                retained.extend_from_slice(&buffer[..size.min(available)]);
            }
            Ok(retained)
        });
        Self { count, limit, thread }
    }

    fn exceeded(&self) -> bool {
        self.count.load(Ordering::Acquire) > self.limit
    }

    fn finish(self) -> std::io::Result<Drained> {
        let exceeded = self.exceeded();
        let bytes = self
            .thread
            .join()
            .map_err(|_| std::io::Error::other("subprocess capture thread panicked"))??;
        Ok(Drained { bytes, exceeded })
    }
}

struct OwnedChild {
    child: Child,
    group: u32,
    reaped: bool,
}

impl OwnedChild {
    fn new(child: Child) -> Self {
        let group = child.id();
        Self {
            child,
            group,
            reaped: false,
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let status = self.child.try_wait()?;
        self.reaped |= status.is_some();
        Ok(status)
    }

    fn terminate(&mut self) -> std::io::Result<()> {
        self.signal(libc::SIGTERM)?;
        let deadline = Instant::now() + TERM_GRACE;
        while !self.reaped && Instant::now() < deadline {
            match self.try_wait() {
                Ok(None) => thread::sleep(POLL),
                Ok(Some(_)) | Err(_) => break,
            }
        }
        self.signal(libc::SIGKILL)?;
        let _ = self.child.kill();
        if !self.reaped {
            self.child.wait()?;
            self.reaped = true;
        }
        self.quiesce()
    }

    fn quiesce(&self) -> std::io::Result<()> {
        self.signal(libc::SIGTERM)?;
        let deadline = Instant::now() + TERM_GRACE;
        while self.group_exists() {
            if Instant::now() >= deadline {
                self.signal(libc::SIGKILL)?;
                let kill_deadline = Instant::now() + TERM_GRACE;
                while self.group_exists() {
                    if Instant::now() >= kill_deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "host subprocess group did not quiesce",
                        ));
                    }
                    thread::sleep(POLL);
                }
                return Ok(());
            }
            thread::sleep(POLL);
        }
        Ok(())
    }

    fn signal(&self, signal: i32) -> std::io::Result<bool> {
        let group =
            i32::try_from(self.group).map_err(|_| std::io::Error::other("subprocess group exceeded host pid range"))?;
        // SAFETY: a negative, validated process-group ID and integer signal do
        // not reference Rust memory. The kernel owns process identity, and the
        // call cannot unwind or retain an alias.
        let result = unsafe { libc::kill(-group, signal) };
        if result == 0 {
            Ok(true)
        } else {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(false)
            } else {
                Err(error)
            }
        }
    }

    fn group_exists(&self) -> bool {
        self.signal(0).unwrap_or(true)
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.reaped || self.group_exists() {
            let _ = self.terminate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn capture(directory: &tempfile::TempDir) -> Capture {
        Capture {
            stdout: directory.path().join("stdout"),
            stderr: directory.path().join("stderr"),
            stdout_limit: 1024,
            stderr_limit: 1024,
        }
    }

    #[test]
    fn completes_and_captures() {
        let directory = tempfile::tempdir().unwrap();
        let outcome = run(
            OwnedCommand::new("sh").args(["-c", "printf owned"]),
            &capture(&directory),
            Duration::from_secs(2),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::Exited(Some(0)));
        assert_eq!(fs::read(directory.path().join("stdout")).unwrap(), b"owned");
    }

    #[test]
    fn timeout_kills_group_without_pipe_wait() {
        let directory = tempfile::tempdir().unwrap();
        let outcome = run(
            OwnedCommand::new("sh").args(["-c", "sleep 60 & echo $!; wait"]),
            &capture(&directory),
            Duration::from_millis(30),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::TimedOut);
        assert_reported_process_gone(&directory.path().join("stdout"));
    }

    #[test]
    fn cancellation_kills_group() {
        let directory = tempfile::tempdir().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let setting = Arc::clone(&cancelled);
        let setter = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            setting.store(true, Ordering::Release);
        });
        let outcome = run(
            OwnedCommand::new("sh").args(["-c", "sleep 60 & wait"]),
            &capture(&directory),
            Duration::from_secs(2),
            &cancelled,
        )
        .unwrap();
        setter.join().unwrap();
        assert_eq!(outcome, Outcome::Cancelled);
    }

    #[test]
    fn output_is_bounded_without_blocking_on_a_pipe() {
        let directory = tempfile::tempdir().unwrap();
        let mut capture = capture(&directory);
        capture.stdout_limit = 10;
        let outcome = run(
            OwnedCommand::new("sh").args(["-c", "while :; do printf 1234567890; done"]),
            &capture,
            Duration::from_secs(2),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::OutputLimit);
        assert_eq!(fs::metadata(directory.path().join("stdout")).unwrap().len(), 10);
    }

    #[test]
    fn signal_identity_is_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let outcome = run(
            OwnedCommand::new("sh").args(["-c", "kill -ABRT $$"]),
            &capture(&directory),
            Duration::from_secs(2),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::Signaled(libc::SIGABRT));
    }

    #[test]
    fn natural_leader_exit_quiesces_lingering_group() {
        let directory = tempfile::tempdir().unwrap();
        let outcome = run(
            OwnedCommand::new("sh").args(["-c", "sleep 60 & echo $!; exit 0"]),
            &capture(&directory),
            Duration::from_secs(2),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::Exited(Some(0)));
        assert_reported_process_gone(&directory.path().join("stdout"));
    }

    fn assert_reported_process_gone(path: &std::path::Path) {
        let process = fs::read_to_string(path).unwrap().trim().parse::<i32>().unwrap();
        // SAFETY: signal zero only queries the numeric PID and retains no Rust
        // storage. ESRCH proves the recorded descendant identity is gone.
        let result = unsafe { libc::kill(process, 0) };
        assert_eq!(result, -1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }
}
