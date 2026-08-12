//! Guest-observable process-liveness probes shared by daemon integration tests.

use std::{path::Path, time::Duration};
use tokio::time::sleep;

async fn heartbeat_size(path: &Path) -> Result<Option<u64>, std::io::Error> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => Ok(Some(metadata.len())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) async fn wait_changing(path: &Path, kind: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut previous = None;
    for _ in 0..300 {
        let current = heartbeat_size(path).await?;
        if previous.is_some() && current > previous {
            return Ok(());
        }
        previous = current;
        sleep(Duration::from_millis(10)).await;
    }
    Err(format!("{kind} did not publish a changing heartbeat at {}", path.display()).into())
}

pub(crate) async fn wait_stopped(path: &Path, kind: &str) -> Result<(), Box<dyn std::error::Error>> {
    const STABLE_SAMPLES: usize = 50;
    let mut previous = heartbeat_size(path).await?;
    let mut stable = 0;
    for _ in 0..300 {
        sleep(Duration::from_millis(10)).await;
        let current = heartbeat_size(path).await?;
        if current == previous {
            stable += 1;
            if stable == STABLE_SAMPLES {
                return Ok(());
            }
        } else {
            previous = current;
            stable = 0;
        }
    }
    Err(format!("{kind} kept changing {} after domain termination", path.display()).into())
}
