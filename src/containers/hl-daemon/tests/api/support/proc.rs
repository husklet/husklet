//! Host process probes shared by daemon integration tests.

use std::{path::Path, process::Stdio, time::Duration};
use tokio::{process::Command, time::sleep};

pub(crate) async fn alive(pid: u32) -> Result<bool, std::io::Error> {
    Ok(Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?
        .success())
}

pub(crate) async fn read_pid(path: &Path) -> Result<u32, Box<dyn std::error::Error>> {
    for _ in 0..300 {
        if let Ok(text) = tokio::fs::read_to_string(path).await
            && let Ok(pid) = text.trim().parse() {
                return Ok(pid);
            }
        sleep(Duration::from_millis(10)).await;
    }
    Err(format!("{} was not published", path.display()).into())
}

pub(crate) async fn wait_dead(pid: u32, kind: &str) -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..300 {
        if !alive(pid).await? {
            return Ok(());
        }
        sleep(Duration::from_millis(10)).await;
    }
    Err(format!("{kind} {pid} survived domain termination").into())
}
