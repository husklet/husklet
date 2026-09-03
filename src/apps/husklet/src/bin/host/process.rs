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
        ffi::Signal::close_tree(self.0, std::time::Duration::from_millis(200))
    }
}

impl ProcessId {
    fn close(&self) -> std::io::Result<()> {
        self.signal(|process| ffi::Signal::close_tree(process, std::time::Duration::from_millis(200)))
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
                && Processes::launches(&fields, &self.workspace)
        })
    }
}

impl Processes {
    /// Reclaims every host launcher process that still belongs to one workspace.
    ///
    /// The domain socket is not an inventory: a launcher can outlive a crashed or replaced domain.
    /// Discover all exact workspace launch identities first, then revalidate each PID against a
    /// fresh process snapshot immediately before signalling it so PID reuse cannot target an
    /// unrelated process.
    pub fn close_workspace(workspace: &str) -> std::io::Result<()> {
        let snapshot = Self::snapshot()?;
        let closed = Self::close_workspace_with(&snapshot, workspace, ProcessId::close);
        Self::wait_workspace_closed_with(workspace, closed, std::time::Duration::from_secs(2), Self::snapshot)
    }

    fn wait_workspace_closed_with(
        workspace: &str,
        delivered: std::io::Result<()>,
        timeout: std::time::Duration,
        mut snapshot: impl FnMut() -> std::io::Result<String>,
    ) -> std::io::Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if Self::workspace_launchers(&snapshot()?, workspace).is_empty() {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return match delivered {
                    Err(error) => Err(error),
                    Ok(()) => Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("workspace launcher processes for {workspace:?} did not exit"),
                    )),
                };
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn close_workspace_with(
        snapshot: &str,
        workspace: &str,
        mut close: impl FnMut(&ProcessId) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let mut failure = None;
        for process in Self::workspace_launchers(snapshot, workspace) {
            match close(&process) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) if failure.is_none() => failure = Some(error),
                Err(_) => {}
            }
        }
        failure.map_or(Ok(()), Err)
    }

    fn workspace_launchers(snapshot: &str, workspace: &str) -> Vec<ProcessId> {
        snapshot
            .lines()
            .filter_map(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                let value = fields.first()?.parse().ok()?;
                Self::launches(&fields, workspace).then(|| ProcessId {
                    value,
                    workspace: workspace.to_owned(),
                })
            })
            .filter(|process| process.value > 1)
            .collect()
    }

    fn launches(fields: &[&str], workspace: &str) -> bool {
        fields.windows(4).any(|values| {
            std::path::Path::new(values[0])
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                == Some("husklet")
                && values[1..] == ["--worker", "launch", workspace]
        })
    }

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
    // Process and group signalling in this boundary are `unsafe` libc entry points.
    #![allow(unsafe_code)]

    use std::io;

    pub(super) struct Signal;

    impl Signal {
        pub(super) fn close_tree(process: i32, grace: std::time::Duration) -> io::Result<()> {
            match Self::hangup(process) {
                Ok(()) => {}
                Err(error) if error.raw_os_error() == Some(libc::ESRCH) => return Ok(()),
                Err(error) => return Err(error),
            }
            let deadline = std::time::Instant::now() + grace;
            while std::time::Instant::now() < deadline {
                if !Self::alive(process) {
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            match Self::send(process, libc::SIGKILL) {
                Ok(()) => Ok(()),
                Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
                Err(error) => Err(error),
            }
        }

        fn alive(process: i32) -> bool {
            // SAFETY: signal zero probes integer process identities without delivering a signal.
            unsafe { libc::kill(-process, 0) == 0 || libc::kill(process, 0) == 0 }
        }

        pub(super) fn hangup(group: i32) -> io::Result<()> {
            Self::hangup_with(group, |target| {
                // SAFETY: `kill` consumes only integers and no Rust storage. A negative target
                // addresses the process group; a positive target closes the pre-setsid race.
                Self::result(unsafe { libc::kill(target, libc::SIGHUP) })
            })
        }

        fn send(process: i32, signal: i32) -> io::Result<()> {
            Self::send_with(process, signal, |target, signal| {
                // SAFETY: `kill` consumes only integers and no Rust storage. The kernel
                // owns concurrent delivery, and this non-unwinding C call retains no state.
                Self::result(unsafe { libc::kill(target, signal) })
            })
        }

        pub(super) fn hangup_with(process: i32, mut deliver: impl FnMut(i32) -> io::Result<()>) -> io::Result<()> {
            Self::validate(process)?;
            let group = deliver(-process);
            let process = deliver(process);
            match (group, process) {
                (Err(_), Err(error)) => Err(error),
                _ => Ok(()),
            }
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

        #[cfg(test)]
        pub(super) fn prepare_session(command: &mut std::process::Command) {
            use std::os::unix::process::CommandExt as _;
            // SAFETY: the closure invokes only the async-signal-safe setsid boundary before exec.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
        }

        #[cfg(test)]
        pub(super) fn force(process: i32) -> io::Result<()> {
            Self::send(process, libc::SIGKILL)
        }
    }
}

#[cfg(not(unix))]
mod ffi {
    use std::io;

    pub(super) struct Signal;

    impl Signal {
        /// `ProcessGroup::hangup` and `Processes::close` call this on every host, so the non-POSIX
        /// arm has to answer it. It was missing, and the two call sites carry no `#[cfg]` of their
        /// own, so this module was narrower than its consumers -- the same shape that cost 73
        /// Windows errors in `hl-engine`'s `runtime::terminal`.
        pub(super) fn close_tree(_process: i32, _grace: std::time::Duration) -> io::Result<()> {
            Self::unsupported()
        }

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
    use super::{ProcessGroup, ProcessId, Processes};

    #[test]
    fn snapshot_identity() {
        let process = ProcessId {
            value: 42,
            workspace: "design%20system".into(),
        };
        assert!(process.matches("42 1 00:01 /x/husklet --worker launch design%20system pane"));
        assert!(!process.matches("42 1 00:01 /usr/bin/python unrelated.py"));
        assert!(!process.matches("42 1 00:01 /x/husklet --worker launch design pane"));
    }

    #[test]
    fn workspace_cleanup_targets_every_exact_launcher_and_ignores_name_prefixes() {
        let snapshot = "42 1 00:01 /x/husklet --worker launch demo pane-1\n\
                        43 42 00:01 /x/husklet --worker launch demo pane-1\n\
                        44 1 00:01 /x/husklet --worker launch demo-next pane-1\n\
                        45 1 00:01 /usr/bin/tool --worker launch demo pane-1";
        let mut closed = Vec::new();

        Processes::close_workspace_with(snapshot, "demo", |process| {
            closed.push(process.value);
            Ok(())
        })
        .unwrap();

        assert_eq!(closed, [42, 43]);
    }

    #[test]
    fn workspace_cleanup_is_idempotent_and_attempts_every_discovered_launcher() {
        let snapshot = "42 1 00:01 /x/husklet --worker launch demo pane-1\n\
                        43 1 00:01 /x/husklet --worker launch demo pane-2\n\
                        44 1 00:01 /x/husklet --worker launch demo pane-3";
        let mut closed = Vec::new();

        let error = Processes::close_workspace_with(snapshot, "demo", |process| {
            closed.push(process.value);
            match process.value {
                42 => Err(std::io::Error::new(std::io::ErrorKind::NotFound, "already exited")),
                43 => Err(std::io::Error::from_raw_os_error(libc::EPERM)),
                _ => Ok(()),
            }
        })
        .unwrap_err();

        assert_eq!(closed, [42, 43, 44]);
        assert_eq!(error.raw_os_error(), Some(libc::EPERM));
    }

    #[test]
    fn cleanup_waits_after_successful_signal_delivery_until_the_launcher_is_absent() {
        let live = "42 1 00:01 /x/husklet --worker launch demo pane-1";
        let mut snapshots = [live.to_owned(), live.to_owned(), String::new()].into_iter();
        Processes::wait_workspace_closed_with("demo", Ok(()), std::time::Duration::from_secs(1), || {
            Ok(snapshots.next().expect("cleanup polled beyond confirmed exit"))
        })
        .unwrap();
        assert!(snapshots.next().is_none());
    }

    #[test]
    fn cleanup_preserves_delivery_error_when_verified_descendants_persist() {
        let live = "42 1 00:01 /x/husklet --worker launch demo pane-1".to_owned();
        let error = Processes::wait_workspace_closed_with(
            "demo",
            Err(std::io::Error::from_raw_os_error(libc::EPERM)),
            std::time::Duration::ZERO,
            || Ok(live.clone()),
        )
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EPERM));
    }

    #[test]
    fn cleanup_success_that_never_settles_becomes_a_timeout() {
        let live = "42 1 00:01 /x/husklet --worker launch demo pane-1".to_owned();
        let error =
            Processes::wait_workspace_closed_with("demo", Ok(()), std::time::Duration::ZERO, || Ok(live.clone()))
                .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
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
    fn hangup_targets_the_tree_and_the_verified_worker() {
        let mut calls = Vec::new();
        Signal::hangup_with(42, |process| {
            calls.push(process);
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, [-42, 42]);
    }

    #[cfg(unix)]
    #[test]
    fn hangup_is_idempotent_when_either_identity_is_already_gone() {
        for failed in [-42, 42] {
            Signal::hangup_with(42, |process| {
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
    fn close_reaps_a_real_session_that_ignores_hangup_within_the_bound() {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "trap '' HUP; while :; do sleep 60; done"]);
        Signal::prepare_session(&mut command);
        let mut child = command.spawn().unwrap();
        let group = ProcessGroup::new(i32::try_from(child.id()).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(50));
        let started = std::time::Instant::now();

        group.hangup().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                let _ = Signal::force(i32::try_from(child.id()).unwrap());
                let _ = child.wait();
                panic!("terminal process tree survived its bounded close");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };

        assert!(!status.success());
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        group.hangup().unwrap();
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
