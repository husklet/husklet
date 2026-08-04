use super::Error;
use nix::{
    sys::signal::{killpg, Signal},
    unistd::Pid,
};
use tokio::process::{Child, Command};

pub(super) struct ProcessGroup {
    child: Child,
    id: i32,
    active: bool,
}

impl ProcessGroup {
    pub(super) fn spawn(command: &mut Command) -> std::io::Result<Self> {
        command.process_group(0).kill_on_drop(true);
        let child = command.spawn()?;
        let id =
            i32::try_from(child.id().ok_or_else(|| {
                std::io::Error::other("spawned scenario process has no process id")
            })?)
            .map_err(|_| std::io::Error::other("scenario process id exceeds i32"))?;
        Ok(Self {
            child,
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
        if self.active {
            let _ = killpg(Pid::from_raw(self.id), Signal::SIGKILL);
        }
    }
}

pub(super) async fn test_timeout_reaps_owned_process_group() -> Result<(), Error> {
    use std::{path::Path, time::Duration};

    let temporary = tempfile::tempdir()?;
    let helper_pid = temporary.path().join("helper.pid");
    let mut unrelated = Command::new("sleep").arg("120").spawn()?;
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "sleep 120 & echo $! > \"$1\"; wait", "scenario"])
        .arg(&helper_pid);
    let process = ProcessGroup::spawn(&mut command)?;
    wait_for_path(&helper_pid, true).await;
    let pid = std::fs::read_to_string(&helper_pid)?
        .trim()
        .parse::<u32>()?;

    assert!(
        tokio::time::timeout(Duration::from_millis(20), process.wait())
            .await
            .is_err()
    );
    wait_for_path(Path::new(&format!("/proc/{pid}")), false).await;
    assert!(unrelated.try_wait()?.is_none());
    unrelated.kill().await?;
    unrelated.wait().await?;
    Ok(())
}

async fn wait_for_path(path: &std::path::Path, exists: bool) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while path.exists() != exists {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
