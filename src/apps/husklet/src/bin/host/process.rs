pub struct Processes;

#[derive(Clone, Copy)]
pub struct ProcessId(i32);

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
    pub fn parse(value: &str) -> Self {
        Self(value.parse().unwrap_or_default())
    }

    pub fn terminate(self) {
        self.signal(libc::SIGTERM);
    }

    pub fn kill(self) {
        self.signal(libc::SIGKILL);
    }

    fn signal(self, signal: i32) {
        if self.0 <= 1 {
            return;
        }
        // SAFETY: supported hosts are POSIX; a negative PID addresses the process group.
        unsafe {
            libc::kill(-self.0, signal);
            libc::kill(self.0, signal);
        }
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
