// The process control primitives this module wraps are all `unsafe` libc entry points.
#![allow(unsafe_code)]

use super::{Capture, Command as OwnedCommand, Outcome};
use crate::drain::Drain;
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(10);
const TERM_GRACE: Duration = Duration::from_millis(500);
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

mod child;
mod spawn;
#[cfg(target_os = "linux")]
mod tree;

use child::{OwnedChild, Process, nonblocking};
use spawn::spawn;

pub(super) fn run(
    command: &OwnedCommand,
    capture: &Capture,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> std::io::Result<Outcome> {
    let spawned = spawn(command)?;
    let mut owned = OwnedChild::new(spawned.process);
    nonblocking(&spawned.stdout)?;
    nonblocking(&spawned.stderr)?;
    let stdout = Drain::spawn(spawned.stdout, capture.stdout_limit);
    let stderr = Drain::spawn(spawned.stderr, capture.stderr_limit);
    supervise(&mut owned, stdout, stderr, capture, timeout, cancelled)
}

fn supervise(
    owned: &mut OwnedChild,
    stdout: Drain,
    stderr: Drain,
    capture: &Capture,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> std::io::Result<Outcome> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EnvironmentEntry;
    use std::cell::Cell;
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;
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
    fn exact_environment_preserves_order_duplicates_and_non_utf8() {
        let directory = tempfile::tempdir().unwrap();
        let environment = [
            EnvironmentEntry::new(b"HL_PROCESS_ENV_CHILD", b"1").unwrap(),
            EnvironmentEntry::new(b"FIRST", b"one").unwrap(),
            EnvironmentEntry::new(b"DUP", b"first").unwrap(),
            EnvironmentEntry::new(b"RAW", b"\xff").unwrap(),
            EnvironmentEntry::new(b"DUP", b"second").unwrap(),
            EnvironmentEntry::new(b"EMPTY", b"").unwrap(),
        ];
        let mut command = OwnedCommand::new(std::env::current_exe().unwrap());
        command.args(["--exact", "unix::tests::exact_environment_child", "--ignored"]);
        command.exact_environment(environment).unwrap();
        let outcome = run(
            &command,
            &capture(&directory),
            Duration::from_secs(2),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_child_exited_zero(outcome, &directory);
    }

    #[test]
    fn exact_spawn_survives_closed_standard_descriptors() {
        let directory = tempfile::tempdir().unwrap();
        let mut command = OwnedCommand::new(std::env::current_exe().unwrap());
        command.args(["--exact", "unix::tests::closed_standard_descriptors_child", "--ignored"]);
        command
            .exact_environment([EnvironmentEntry::new(b"HL_PROCESS_CLOSED_CHILD", b"1").unwrap()])
            .unwrap();
        let outcome = run(
            &command,
            &capture(&directory),
            Duration::from_secs(3),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_child_exited_zero(outcome, &directory);
    }

    /// The captured streams are bounded at 1 KiB, so a failing child's libtest panic is itself large
    /// enough to be reported as `OutputLimit`. Quote what was captured, or the outcome names the
    /// symptom and hides the assertion that produced it.
    fn assert_child_exited_zero(outcome: Outcome, directory: &tempfile::TempDir) {
        assert_eq!(
            outcome,
            Outcome::Exited(Some(0)),
            "child stdout: {}\nchild stderr: {}",
            String::from_utf8_lossy(&fs::read(directory.path().join("stdout")).unwrap_or_default()),
            String::from_utf8_lossy(&fs::read(directory.path().join("stderr")).unwrap_or_default())
        );
    }

    #[test]
    #[ignore = "executed with standard descriptors closed"]
    fn closed_standard_descriptors_child() {
        let directory = tempfile::tempdir().unwrap();
        let environment = [
            EnvironmentEntry::new(b"HL_PROCESS_ENV_CHILD", b"1").unwrap(),
            EnvironmentEntry::new(b"FIRST", b"one").unwrap(),
            EnvironmentEntry::new(b"DUP", b"first").unwrap(),
            EnvironmentEntry::new(b"RAW", b"\xff").unwrap(),
            EnvironmentEntry::new(b"DUP", b"second").unwrap(),
            EnvironmentEntry::new(b"EMPTY", b"").unwrap(),
        ];
        // This isolated subprocess deliberately relinquishes its three standard descriptors
        // before exercising descriptor relocation.
        // SAFETY: it performs no Rust I/O afterward, terminates with `_exit`, owns no shared
        // process state, and no unwind crosses FFI.
        unsafe {
            libc::close(libc::STDIN_FILENO);
            libc::close(libc::STDOUT_FILENO);
            libc::close(libc::STDERR_FILENO);
        }
        let mut command = OwnedCommand::new(std::env::current_exe().unwrap());
        command.args(["--exact", "unix::tests::exact_environment_child", "--ignored"]);
        let success = command.exact_environment(environment).is_ok()
            && matches!(
                run(
                    &command,
                    &capture(&directory),
                    Duration::from_secs(2),
                    &AtomicBool::new(false),
                ),
                Ok(Outcome::Exited(Some(0)))
            );
        // SAFETY: this is the terminal action of the isolated child test. It
        // runs no destructors, retains no pointer, and cannot unwind.
        unsafe { libc::_exit(if success { 0 } else { 101 }) }
    }

    #[test]
    #[ignore = "executed as the controlled exact-environment subprocess"]
    fn exact_environment_child() {
        assert_eq!(
            exec_environment(),
            [
                &b"HL_PROCESS_ENV_CHILD=1"[..],
                &b"FIRST=one"[..],
                &b"DUP=first"[..],
                &b"RAW=\xff"[..],
                &b"DUP=second"[..],
                &b"EMPTY="[..],
            ]
        );
    }

    /// Darwin's libSystem calls `setenv("__CF_USER_TEXT_ENCODING", ...)` before `main` whenever the
    /// variable is absent, so a child's `environ` is never byte-identical to the `envp` it was
    /// `posix_spawn`ed with. That injection is the host's, not this crate's: dropping exactly that
    /// record leaves every property the exec contract owns -- order, duplicate retention, non-UTF-8
    /// bytes, an empty value, and the absence of every inherited variable -- still asserted below.
    fn exec_environment() -> Vec<Vec<u8>> {
        let mut records = raw_environment();
        if cfg!(target_os = "macos") {
            let injected = records
                .iter()
                .position(|record| record.starts_with(b"__CF_USER_TEXT_ENCODING="));
            assert!(
                injected.is_some(),
                "Darwin no longer injects __CF_USER_TEXT_ENCODING; drop this allowance and assert the raw environment"
            );
            records.remove(injected.expect("checked above"));
        }
        records
    }

    fn raw_environment() -> Vec<Vec<u8>> {
        let mut records = Vec::new();
        // SAFETY: the process owns a conventional NUL-terminated `environ`
        // pointer table for its entire lifetime. This test only observes each
        // immutable C string before any thread mutates the environment; no
        // pointer escapes and CStr validation cannot unwind across FFI.
        let mut cursor = unsafe { environment_pointer() };
        // SAFETY: the table is terminated by a null pointer and each non-null
        // entry addresses one NUL-terminated environment record owned by libc.
        while !unsafe { *cursor }.is_null() {
            // SAFETY: established above; the record is observed and copied
            // immediately, with no mutation or retained alias.
            records.push(unsafe { std::ffi::CStr::from_ptr(*cursor) }.to_bytes().to_vec());
            // SAFETY: the current entry is non-null, so advancing by one stays
            // within the table or reaches its required null terminator.
            cursor = unsafe { cursor.add(1) };
        }
        records
    }

    #[cfg(target_os = "macos")]
    unsafe fn environment_pointer() -> *mut *mut libc::c_char {
        unsafe extern "C" {
            fn _NSGetEnviron() -> *mut *mut *mut libc::c_char;
        }
        // SAFETY: Darwin returns the address of its process-lifetime environ
        // pointer. The caller observes the pointed-to table without mutation.
        unsafe { *_NSGetEnviron() }
    }

    #[cfg(not(target_os = "macos"))]
    unsafe fn environment_pointer() -> *mut *mut libc::c_char {
        unsafe extern "C" {
            static mut environ: *mut *mut libc::c_char;
        }
        // SAFETY: POSIX libc owns this process-lifetime pointer; the caller only
        // observes its table before any environment mutation.
        unsafe { environ }
    }

    #[test]
    fn timeout_kills_group_without_pipe_wait() {
        let directory = tempfile::tempdir().unwrap();
        // The deadline must outlast `sh` reaching its `echo` -- under a loaded box a 30 ms budget
        // terminated the group before the pid was ever written and the test failed parsing an empty
        // capture. It stays far below the 60 s the held pipe would cost, and the elapsed bound below
        // is what actually pins that supervision never waits for the writer.
        let started = Instant::now();
        let outcome = run(
            OwnedCommand::new("sh").args(["-c", "sleep 60 & echo $!; wait"]),
            &capture(&directory),
            Duration::from_millis(250),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "supervision waited for the held pipe"
        );
        assert_reported_process_gone(&directory.path().join("stdout"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn timeout_does_not_wait_for_detached_capture_writer() {
        let directory = tempfile::tempdir().unwrap();
        let detached = DetachedChild::new(directory.path().join("detached.pid"));
        let mut command = OwnedCommand::new(std::env::current_exe().unwrap());
        command.args(["--exact", "unix::tests::detached_capture_writer_child", "--ignored"]);
        command
            .exact_environment([EnvironmentEntry::new(
                b"HL_PROCESS_DETACHED_PID_FILE",
                detached.path.as_os_str().as_bytes(),
            )
            .unwrap()])
            .unwrap();
        let started = Instant::now();
        let outcome = run(
            &command,
            &capture(&directory),
            Duration::from_millis(250),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::TimedOut);
        assert!(started.elapsed() < Duration::from_millis(1_500));
        assert!(
            fs::read(directory.path().join("stdout"))
                .unwrap()
                .ends_with(b"detached\n")
        );
        detached.assert_not_live().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "executed as a subprocess retaining a detached capture writer"]
    fn detached_capture_writer_child() {
        let path = std::env::var_os("HL_PROCESS_DETACHED_PID_FILE").unwrap();
        let report = File::create(path).unwrap();
        // The parent remains in its original process group until supervision terminates it; the
        // child creates a new session and deliberately keeps inherited stdout/stderr open long
        // enough to expose an unbounded drain.
        // SAFETY: the fork child invokes only async-signal-safe libc operations, owns no Rust
        // references after the fork, and terminates with `_exit`.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            // SAFETY: this fork child is not a process-group leader, so setsid
            // creates a detached session without touching Rust storage.
            if unsafe { libc::setsid() } < 0 {
                // SAFETY: this is the terminal action of the isolated child.
                unsafe { libc::_exit(101) };
            }
            let marker = b"detached\n";
            let mut digits = [0_u8; 32];
            // SAFETY: `getpid` takes no argument, touches no Rust storage, and cannot fail.
            let mut value = unsafe { libc::getpid() } as u32;
            let mut cursor = digits.len();
            while value != 0 {
                cursor -= 1;
                digits[cursor] = b'0' + (value % 10) as u8;
                value /= 10;
            }
            // Stdout remains an inherited live descriptor, and the report descriptor was opened
            // before the fork.
            // SAFETY: marker and digits are immutable storage valid for their complete writes,
            // and write, close, sleep and `_exit` retain no Rust pointers and cannot unwind.
            unsafe {
                libc::write(libc::STDOUT_FILENO, marker.as_ptr().cast(), marker.len());
                libc::write(
                    report.as_raw_fd(),
                    digits[cursor..].as_ptr().cast(),
                    digits.len() - cursor,
                );
                libc::close(report.as_raw_fd());
                libc::sleep(2);
                libc::_exit(0);
            }
        }
        std::thread::sleep(Duration::from_secs(60));
    }

    struct DetachedChild {
        path: PathBuf,
        armed: Cell<bool>,
    }

    impl DetachedChild {
        fn new(path: PathBuf) -> Self {
            Self {
                path,
                armed: Cell::new(true),
            }
        }

        fn pid(&self) -> std::io::Result<i32> {
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                if let Ok(text) = fs::read_to_string(&self.path)
                    && let Ok(pid) = text.parse()
                {
                    return Ok(pid);
                }
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "detached fixture did not publish its pid",
                    ));
                }
                thread::sleep(POLL);
            }
        }

        fn assert_not_live(&self) -> std::io::Result<()> {
            if !self.armed.get() {
                return Ok(());
            }
            let pid = self.pid()?;
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                let stat = fs::read_to_string(format!("/proc/{pid}/stat"));
                if matches!(stat.as_deref(), Ok(value) if value.rsplit_once(") ").is_some_and(|(_, fields)| fields.starts_with('Z')))
                {
                    self.armed.set(false);
                    return Ok(());
                }
                // SAFETY: signal zero only observes the exact fixture PID and
                // retains no pointer or Rust alias.
                if unsafe { libc::kill(pid, 0) } < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() == Some(libc::ESRCH) {
                        self.armed.set(false);
                        return Ok(());
                    }
                    return Err(error);
                }
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "detached fixture was not reaped",
                    ));
                }
                thread::sleep(POLL);
            }
        }
    }

    impl Drop for DetachedChild {
        fn drop(&mut self) {
            if self.armed.get() {
                let Ok(pid) = self.pid() else { return };
                // Test-failure cleanup only; the positive PID came from the
                // fixture's private report file.
                // SAFETY: SIGKILL carries no Rust pointer and cannot unwind.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
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
