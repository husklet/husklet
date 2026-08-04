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

#[cfg(unix)]
#[allow(unsafe_code)]
mod unix;
#[cfg(windows)]
#[allow(unsafe_code)]
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

/// A subprocess command with the supervisor's inherited environment.
///
/// Environment mutation is intentionally absent. Callers must configure typed
/// settings at their owning boundary before supervision; both platform
/// adapters then inherit exactly that process environment.
#[derive(Clone, Debug)]
pub struct Command {
    program: OsString,
    arguments: Vec<OsString>,
}

impl Command {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            arguments: Vec::new(),
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

    fn standard(&self) -> StdCommand {
        let mut command = StdCommand::new(&self.program);
        command.args(&self.arguments);
        command
    }

    #[cfg(windows)]
    fn program(&self) -> &OsStr {
        &self.program
    }

    #[cfg(windows)]
    fn arguments(&self) -> impl Iterator<Item = &OsStr> {
        self.arguments.iter().map(OsString::as_os_str)
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
