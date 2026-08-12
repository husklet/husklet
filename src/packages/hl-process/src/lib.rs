//! Bounded ownership for a host subprocess and the lifecycle domain it starts.
//!
//! On Unix the domain is a process group. Deliberately calling `setsid` or
//! moving to another process group escapes that primitive; callers must only
//! launch trusted host programs whose descendants preserve the group. On
//! Windows the domain is a Job Object without breakaway permission, which is
//! stronger and cannot be escaped by ordinary descendants.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Command as StdCommand;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const ENVIRONMENT_COUNT_LIMIT: usize = 4096;
const ENVIRONMENT_BYTE_LIMIT: usize = 64 * 1024 * 1024;

mod drain;
#[cfg(unix)]
mod unix;
mod platform;
#[cfg(windows)]
mod windows;

#[derive(Clone, Debug)]
pub struct Capture {
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    pub stdout_limit: u64,
    pub stderr_limit: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Exited(Option<i32>),
    Signaled(i32),
    TimedOut,
    Cancelled,
    OutputLimit,
}

/// A subprocess command with either the inherited environment or an exact,
/// bounded environment vector.
#[derive(Clone, Debug)]
pub struct Command {
    program: OsString,
    arguments: Vec<OsString>,
    environment: Option<Vec<EnvironmentEntry>>,
}

/// One byte-exact environment record for a directly launched host process.
///
/// Records are passed in declaration order. Duplicate names are retained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvironmentEntry {
    record: Vec<u8>,
}

impl EnvironmentEntry {
    /// Constructs `name=value` after validating the host `execve` record.
    pub fn new(name: impl AsRef<[u8]>, value: impl AsRef<[u8]>) -> std::io::Result<Self> {
        let name = name.as_ref();
        let value = value.as_ref();
        if name.is_empty() || name.contains(&b'=') || name.contains(&0) || value.contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "environment names must be non-empty and exclude '=' and NUL; values must exclude NUL",
            ));
        }
        let mut record = Vec::with_capacity(name.len() + value.len() + 1);
        record.extend_from_slice(name);
        record.push(b'=');
        record.extend_from_slice(value);
        Ok(Self { record })
    }

    #[cfg(unix)]
    fn record(&self) -> &[u8] {
        &self.record
    }

    fn size(&self) -> usize {
        self.record.len() + 1
    }
}

impl Command {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            arguments: Vec::new(),
            environment: None,
        }
    }

    pub fn arg(&mut self, argument: impl AsRef<OsStr>) -> &mut Self {
        self.arguments.push(argument.as_ref().to_owned());
        self
    }

    pub fn args<I, S>(&mut self, arguments: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.arguments
            .extend(arguments.into_iter().map(|value| value.as_ref().to_owned()));
        self
    }

    /// Replaces inheritance with an exact ordered environment vector.
    ///
    /// Unix preserves byte values, declaration order, and duplicate names.
    /// Windows currently returns `Unsupported` when this capability is used.
    pub fn exact_environment<I>(&mut self, environment: I) -> std::io::Result<&mut Self>
    where
        I: IntoIterator<Item = EnvironmentEntry>,
    {
        let environment = environment.into_iter().collect::<Vec<_>>();
        let bytes = environment
            .iter()
            .try_fold(0_usize, |total, entry| total.checked_add(entry.size()));
        validate_environment(environment.len(), bytes)?;
        self.environment = Some(environment);
        Ok(self)
    }

    fn standard(&self) -> StdCommand {
        let mut command = platform::command(&self.program);
        command.args(&self.arguments);
        command
    }

    fn environment(&self) -> Option<&[EnvironmentEntry]> {
        self.environment.as_deref()
    }

    #[cfg(any(unix, windows))]
    fn program(&self) -> &OsStr {
        &self.program
    }

    #[cfg(any(unix, windows))]
    fn arguments(&self) -> impl Iterator<Item = &OsStr> {
        self.arguments.iter().map(OsString::as_os_str)
    }
}

fn validate_environment(count: usize, bytes: Option<usize>) -> std::io::Result<()> {
    if count > ENVIRONMENT_COUNT_LIMIT || bytes.is_none_or(|bytes| bytes > ENVIRONMENT_BYTE_LIMIT) {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "exact process environment exceeds 4096 records or 64 MiB",
        ))
    } else {
        Ok(())
    }
}

/// Runs the command to completion while owning its process domain and captures.
///
/// # Errors
///
/// Returns a host I/O error when creation, supervision, teardown, reaping, or
/// writing either bounded capture fails.
pub fn run(
    command: &Command,
    capture: &Capture,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> std::io::Result<Outcome> {
    #[cfg(unix)]
    return unix::run(command, capture, timeout, cancelled);
    #[cfg(windows)]
    return windows::run(command, capture, timeout, cancelled);
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (command, capture, timeout, cancelled);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "host subprocess supervision is unavailable on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, ENVIRONMENT_BYTE_LIMIT, ENVIRONMENT_COUNT_LIMIT, EnvironmentEntry, validate_environment};

    #[test]
    fn exact_environment_rejects_invalid_exec_records() {
        for (name, value) in [
            (&b""[..], &b"value"[..]),
            (&b"BAD=NAME"[..], &b"value"[..]),
            (&b"BAD\0NAME"[..], &b"value"[..]),
            (&b"NAME"[..], &b"bad\0value"[..]),
        ] {
            assert!(EnvironmentEntry::new(name, value).is_err());
        }
    }

    #[test]
    fn exact_environment_accepts_empty_and_rejects_count_and_byte_overflow() {
        let mut command = Command::new("program");
        command.exact_environment([]).unwrap();
        assert!(command.environment().unwrap().is_empty());
        assert!(validate_environment(ENVIRONMENT_COUNT_LIMIT + 1, Some(0)).is_err());
        assert!(validate_environment(1, Some(ENVIRONMENT_BYTE_LIMIT + 1)).is_err());
        assert!(validate_environment(1, None).is_err());
    }
}
