use tokio::process::{Child, Command};

#[allow(dead_code, reason = "reserved for typed host-action and image-import subprocesses")]
pub(super) struct ProcessGroup {
    child: Child,
    #[cfg(unix)]
    id: i32,
    active: bool,
}

#[allow(dead_code, reason = "reserved for typed host-action and image-import subprocesses")]
impl ProcessGroup {
    pub(super) fn spawn(command: &mut Command) -> std::io::Result<Self> {
        #[cfg(unix)]
        command.process_group(0);
        command.kill_on_drop(true);
        let child = command.spawn()?;
        #[cfg(unix)]
        let id = i32::try_from(
            child
                .id()
                .ok_or_else(|| std::io::Error::other("spawned scenario process has no process id"))?,
        )
        .map_err(|_| std::io::Error::other("scenario process id exceeds i32"))?;
        Ok(Self {
            child,
            #[cfg(unix)]
            id,
            active: true,
        })
    }

    pub(super) async fn wait(mut self) -> std::io::Result<std::process::ExitStatus> {
        let status = self.child.wait().await;
        if status.is_ok() {
            self.active = false;
        }
        status
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        #[cfg(unix)]
        {
            use nix::{
                sys::signal::{Signal, killpg},
                unistd::Pid,
            };
            let _ = killpg(Pid::from_raw(self.id), Signal::SIGKILL);
        }
        #[cfg(not(unix))]
        {
            let _ = self.child.start_kill();
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::ProcessGroup;
    use std::{path::Path, time::Duration};
    use tokio::process::Command;

    #[tokio::test]
    async fn timeout_reaps_owned_process_group_without_touching_unrelated_child() {
        let temporary = tempfile::tempdir().unwrap();
        let helper_pid = temporary.path().join("helper.pid");
        let mut unrelated = Command::new("sleep").arg("120").spawn().unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "sleep 120 & echo $! > \"$1\"; wait", "scenario"])
            .arg(&helper_pid);
        let process = ProcessGroup::spawn(&mut command).unwrap();
        wait_for_path(&helper_pid, true).await;
        let pid = std::fs::read_to_string(&helper_pid)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), process.wait())
                .await
                .is_err()
        );
        wait_for_path(Path::new(&format!("/proc/{pid}")), false).await;
        assert!(unrelated.try_wait().unwrap().is_none());
        unrelated.kill().await.unwrap();
        unrelated.wait().await.unwrap();
    }

    async fn wait_for_path(path: &Path, exists: bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while path.exists() != exists {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
}
