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

    pub fn hangup(&self) {
        if self.0 > 1 {
            // SAFETY: supported hosts provide POSIX process groups.
            unsafe { libc::killpg(self.0, libc::SIGHUP) };
        }
    }
}

impl ProcessId {
    pub fn parse(value: &str, workspace: &str) -> Option<Self> {
        value
            .parse()
            .ok()
            .filter(|value| *value > 1)
            .map(|value| Self {
                value,
                workspace: workspace.to_owned(),
            })
    }

    pub fn terminate(&self) -> std::io::Result<()> {
        self.signal(libc::SIGTERM)
    }

    pub fn kill(&self) -> std::io::Result<()> {
        self.signal(libc::SIGKILL)
    }

    fn signal(&self, signal: i32) -> std::io::Result<()> {
        let snapshot = Processes::snapshot()?;
        if !self.matches(&snapshot) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "workspace process is no longer active",
            ));
        }
        // SAFETY: supported hosts are POSIX; a negative PID addresses the process group.
        let group = unsafe { libc::kill(-self.value, signal) };
        let process = unsafe { libc::kill(self.value, signal) };
        if group < 0 && process < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
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
}

#[cfg(test)]
mod tests {
    use super::ProcessId;

    #[test]
    fn process_identity_rejects_invalid_and_system_sentinel_values() {
        assert!(ProcessId::parse("42", "runtime").is_some());
        for value in ["", "not-a-pid", "-1", "0", "1"] {
            assert!(ProcessId::parse(value, "runtime").is_none(), "{value}");
        }
    }

    #[test]
    fn stale_process_identity_never_matches_a_recycled_pid() {
        let process = ProcessId::parse("42", "design%20system").unwrap();
        assert!(process.matches("42 1 00:01 /x/husklet --worker launch design%20system pane"));
        assert!(!process.matches("42 1 00:01 /usr/bin/python unrelated.py"));
        assert!(!process.matches("42 1 00:01 /x/husklet --worker launch design pane"));
    }
}
