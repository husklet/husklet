use crate::volumes::Volumes;
use crate::{Error, Result, Volume, VolumeSource};
use hl_fs::Directory;
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::task::JoinSet;

const SCAN_LIMIT: usize = 8;

impl Volumes {
    /// Measure regular-file bytes currently owned by this volume.
    ///
    /// Symbolic links are measured as links and never followed.
    ///
    /// # Errors
    /// Returns validation, lookup, task, or filesystem failures.
    pub async fn size(&self, name: &str) -> Result<u64> {
        let volume = self.inspect(name).await?;
        if matches!(volume.source, VolumeSource::Bind { .. }) {
            return Ok(0);
        }
        let path = volume.path;
        Ok(tokio::task::spawn_blocking(move || Directory::from(path).size())
            .await
            .map_err(|error| Error::Io(std::io::Error::other(error)))??)
    }

    /// Measure an already listed volume inventory with bounded filesystem work.
    ///
    /// Managed volume trees are scanned concurrently, with at most eight host
    /// directory walks in flight. Bind-backed volumes report zero because their
    /// contents remain owned by the host. Results retain deterministic name order.
    ///
    /// # Errors
    /// Returns task or filesystem failures from a managed volume scan.
    pub async fn sizes(&self, volumes: &[Volume]) -> Result<BTreeMap<String, u64>> {
        let mut sizes = BTreeMap::new();
        let mut pending = VecDeque::new();
        for volume in volumes {
            if matches!(volume.source, VolumeSource::Bind { .. }) {
                sizes.insert(volume.name.clone(), 0);
            } else {
                pending.push_back((volume.name.clone(), volume.path.clone()));
            }
        }
        sizes.extend(scan(pending, |path| Directory::from(path).size()).await?);
        Ok(sizes)
    }
}

async fn scan<F>(mut pending: VecDeque<(String, PathBuf)>, measure: F) -> Result<BTreeMap<String, u64>>
where
    F: Fn(PathBuf) -> std::io::Result<u64> + Send + Sync + 'static,
{
    let measure = Arc::new(measure);
    let mut scans = JoinSet::new();
    while scans.len() < SCAN_LIMIT {
        let Some((name, path)) = pending.pop_front() else {
            break;
        };
        let measure = Arc::clone(&measure);
        scans.spawn_blocking(move || (name, measure(path)));
    }

    let mut sizes = BTreeMap::new();
    let mut failure = None;
    while let Some(result) = scans.join_next().await {
        match result {
            Ok((name, Ok(size))) => {
                sizes.insert(name, size);
            }
            Ok((_, Err(error))) => {
                if failure.is_none() {
                    failure = Some(Error::Io(error));
                }
            }
            Err(error) => {
                if failure.is_none() {
                    failure = Some(Error::Io(std::io::Error::other(error)));
                }
            }
        }
        if failure.is_none()
            && let Some((name, path)) = pending.pop_front()
        {
            let measure = Arc::clone(&measure);
            scans.spawn_blocking(move || (name, measure(path)));
        }
    }

    match failure {
        Some(error) => Err(error),
        None => Ok(sizes),
    }
}

#[cfg(test)]
mod tests {
    use super::{SCAN_LIMIT, scan};
    use crate::{Config, Containers, Error, VolumeSpec};
    use std::collections::{BTreeMap, VecDeque};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    #[tokio::test]
    async fn batch_crosses_window() {
        let root = tempfile::tempdir().unwrap();
        let containers = Containers::builder(Config::new(root.path())).build().await.unwrap();
        let mut expected = BTreeMap::new();
        for index in 0..=SCAN_LIMIT {
            let name = format!("volume-{index:02}");
            let volume = containers.volumes().create(VolumeSpec::new(&name)).await.unwrap();
            let payload = vec![b'x'; index + 1];
            std::fs::write(volume.path().join("payload"), &payload).unwrap();
            expected.insert(name, payload.len() as u64);
        }
        let listed = containers.volumes().list().await.unwrap();
        assert_eq!(containers.volumes().sizes(&listed).await.unwrap(), expected);
    }

    #[tokio::test]
    async fn bind_zero_and_failure() {
        let root = tempfile::tempdir().unwrap();
        let bind = root.path().join("bind");
        std::fs::create_dir(&bind).unwrap();
        std::fs::write(bind.join("host-owned"), b"not-accounted").unwrap();
        let containers = Containers::builder(Config::new(root.path().join("state")))
            .build()
            .await
            .unwrap();
        let external = containers
            .volumes()
            .create(VolumeSpec::new("external").bind(&bind, false))
            .await
            .unwrap();
        let missing = containers.volumes().create(VolumeSpec::new("missing")).await.unwrap();
        assert_eq!(
            containers
                .volumes()
                .sizes(std::slice::from_ref(&external))
                .await
                .unwrap(),
            BTreeMap::from([("external".into(), 0)])
        );
        std::fs::remove_dir_all(missing.path()).unwrap();
        assert!(matches!(
            containers.volumes().sizes(&[external, missing]).await,
            Err(Error::Io(_))
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failure_drains_started() {
        let active = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started, blocked) = tokio::sync::oneshot::channel();
        let started = Arc::new(Mutex::new(Some(started)));
        let task_active = Arc::clone(&active);
        let task_gate = Arc::clone(&gate);
        let task = tokio::spawn(scan(
            VecDeque::from([
                ("failure".into(), PathBuf::from("failure")),
                ("blocked".into(), PathBuf::from("blocked")),
            ]),
            move |path| {
                if path == PathBuf::from("failure") {
                    return Err(std::io::Error::other("expected failure"));
                }
                task_active.fetch_add(1, Ordering::SeqCst);
                if let Some(started) = started.lock().unwrap().take() {
                    let _ = started.send(());
                }
                let (lock, wake) = &*task_gate;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
                task_active.fetch_sub(1, Ordering::SeqCst);
                Ok(1)
            },
        ));
        blocked.await.unwrap();
        assert!(!task.is_finished());
        {
            let (lock, wake) = &*gate;
            *lock.lock().unwrap() = true;
            wake.notify_all();
        }
        assert!(matches!(task.await.unwrap(), Err(Error::Io(_))));
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
}
