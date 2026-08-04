pub struct Processes;

#[derive(Clone)]
pub struct ProcessId {
    value: i32,
    workspace: String,
}

pub struct ProcessGroup(i32);

impl ProcessGroup {
    pub fn new(id: i32) -> Self {
        Self(id)
    }

    pub fn hangup(&self) -> std::io::Result<()> {
        ffi::Signal::hangup(self.0)
    }
}

impl ProcessId {
    pub fn parse(value: &str, workspace: &str) -> Option<Self> {
        value.parse().ok().filter(|value| *value > 1).map(|value| Self {
            value,
            workspace: workspace.to_owned(),
        })
    }

    pub fn terminate(&self) -> std::io::Result<()> {
        self.signal(ffi::Signal::terminate)
    }

    pub fn kill(&self) -> std::io::Result<()> {
        self.signal(ffi::Signal::kill)
    }

    fn signal(&self, signal: fn(i32) -> std::io::Result<()>) -> std::io::Result<()> {
        let snapshot = Processes::snapshot()?;
        if !self.matches(&snapshot) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "workspace process is no longer active",
            ));
        }
        signal(self.value)
    }

    fn matches(&self, snapshot: &str) -> bool {
        snapshot.lines().any(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            fields.first().and_then(|value| value.parse::<i32>().ok()) == Some(self.value)
                && fields
                    .windows(3)
                    .any(|values| values == ["--worker", "launch", self.workspace.as_str()])
        })
    }
}

impl Processes {
    #[cfg(unix)]
    pub fn snapshot() -> std::io::Result<String> {
        let output = std::process::Command::new("/bin/ps")
            .args(["-axo", "pid=,ppid=,etime=,command="])
            .output()?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(std::io::Error::other("ps failed"))
        }
    }

    #[cfg(not(unix))]
    pub fn snapshot() -> std::io::Result<String> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "workspace process discovery requires a POSIX host adapter",
        ))
    }
}

#[cfg(unix)]
mod ffi {
    use std::io;

    pub(super) struct Signal;

    impl Signal {
        pub(super) fn hangup(group: i32) -> io::Result<()> {
            Self::hangup_with(group, |group| {
                // SAFETY: `killpg` consumes only integers and no Rust storage. The kernel
                // owns concurrent delivery, and this non-unwinding C call retains no state.
                Self::result(unsafe { libc::killpg(group, libc::SIGHUP) })
            })
        }

        pub(super) fn terminate(process: i32) -> io::Result<()> {
            Self::send(process, libc::SIGTERM)
        }

        pub(super) fn kill(process: i32) -> io::Result<()> {
            Self::send(process, libc::SIGKILL)
        }

        fn send(process: i32, signal: i32) -> io::Result<()> {
            Self::send_with(process, signal, |target, signal| {
                // SAFETY: `kill` consumes only integers and no Rust storage. The kernel
                // owns concurrent delivery, and this non-unwinding C call retains no state.
                Self::result(unsafe { libc::kill(target, signal) })
            })
        }

        pub(super) fn hangup_with(group: i32, mut deliver: impl FnMut(i32) -> io::Result<()>) -> io::Result<()> {
            Self::validate(group)?;
            deliver(group)
        }

        pub(super) fn send_with(
            process: i32,
            signal: i32,
            mut deliver: impl FnMut(i32, i32) -> io::Result<()>,
        ) -> io::Result<()> {
            Self::validate(process)?;
            let group = deliver(-process, signal);
            let process = deliver(process, signal);
            match (group, process) {
                (Err(_), Err(error)) => Err(error),
                _ => Ok(()),
            }
        }

        fn validate(process: i32) -> io::Result<()> {
            if process > 1 {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "process identity must be greater than one",
                ))
            }
        }

        fn result(status: i32) -> io::Result<()> {
            if status < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(not(unix))]
mod ffi {
    use std::io;

    pub(super) struct Signal;

    impl Signal {
        pub(super) fn hangup(_group: i32) -> io::Result<()> {
            Self::unsupported()
        }

        pub(super) fn terminate(_process: i32) -> io::Result<()> {
            Self::unsupported()
        }

        pub(super) fn kill(_process: i32) -> io::Result<()> {
            Self::unsupported()
        }

        fn unsupported() -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "workspace process signaling requires a host adapter",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::ffi::Signal;
    use super::ProcessId;

    #[test]
    fn invalid_identity() {
        assert!(ProcessId::parse("42", "runtime").is_some());
        for value in ["", "not-a-pid", "-1", "0", "1"] {
            assert!(ProcessId::parse(value, "runtime").is_none(), "{value}");
        }
    }

    #[test]
    fn snapshot_identity() {
        let process = ProcessId::parse("42", "design%20system").unwrap();
        assert!(process.matches("42 1 00:01 /x/husklet --worker launch design%20system pane"));
        assert!(!process.matches("42 1 00:01 /usr/bin/python unrelated.py"));
        assert!(!process.matches("42 1 00:01 /x/husklet --worker launch design pane"));
    }

    #[cfg(unix)]
    #[test]
    fn signal_order() {
        let mut calls = Vec::new();
        Signal::send_with(42, libc::SIGTERM, |process, signal| {
            calls.push((process, signal));
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, [(-42, libc::SIGTERM), (42, libc::SIGTERM)]);
    }

    #[cfg(unix)]
    #[test]
    fn partial_success() {
        for failed in [-42, 42] {
            Signal::send_with(42, libc::SIGTERM, |process, _| {
                if process == failed {
                    Err(std::io::Error::from_raw_os_error(libc::ESRCH))
                } else {
                    Ok(())
                }
            })
            .unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn direct_errno() {
        let error = Signal::send_with(42, libc::SIGTERM, |process, _| {
            let errno = if process < 0 { libc::EPERM } else { libc::ESRCH };
            Err(std::io::Error::from_raw_os_error(errno))
        })
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ESRCH));
    }

    #[cfg(unix)]
    #[test]
    fn sentinels_rejected() {
        for process in [i32::MIN, -1, 0, 1] {
            assert_eq!(
                Signal::hangup_with(process, |_| Ok(())).unwrap_err().kind(),
                std::io::ErrorKind::InvalidInput,
            );
            assert_eq!(
                Signal::send_with(process, libc::SIGTERM, |_, _| Ok(()))
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::InvalidInput,
            );
        }
    }
}
