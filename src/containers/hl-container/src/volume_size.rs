use crate::volumes::Volumes;
use crate::{Error, Result, Volume, VolumeSource};
use hl_fs::Directory;
use std::collections::{BTreeMap, HashMap, VecDeque};
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
        for (index, volume) in volumes.iter().enumerate() {
            if matches!(volume.source, VolumeSource::Bind { .. }) {
                sizes.insert(volume.name.clone(), 0);
            } else {
                pending.push_back((index, volume.name.clone(), volume.path.clone()));
            }
        }
        sizes.extend(Self::scan(pending, |path| Directory::from(path).size()).await?);
        Ok(sizes)
    }

    async fn scan<F>(mut pending: VecDeque<(usize, String, PathBuf)>, measure: F) -> Result<BTreeMap<String, u64>>
    where
        F: Fn(PathBuf) -> std::io::Result<u64> + Send + Sync + 'static,
    {
        let measure = Arc::new(measure);
        let mut scans = JoinSet::new();
        let mut indices = HashMap::new();
        while scans.len() < SCAN_LIMIT {
            let Some((index, name, path)) = pending.pop_front() else {
                break;
            };
            Self::enqueue(&mut scans, &mut indices, Arc::clone(&measure), index, name, path);
        }

        let mut sizes = BTreeMap::new();
        let mut failure = None;
        while let Some(result) = scans.join_next_with_id().await {
            match result {
                Ok((id, (_, name, Ok(size)))) => {
                    indices.remove(&id);
                    sizes.insert(name, size);
                }
                Ok((id, (index, _, Err(error)))) => {
                    indices.remove(&id);
                    Self::retain_failure(&mut failure, index, Error::Io(error));
                }
                Err(error) => {
                    let index = indices.remove(&error.id()).unwrap_or(usize::MAX);
                    Self::retain_failure(&mut failure, index, Error::Io(std::io::Error::other(error)));
                }
            }
            if failure.is_none()
                && let Some((index, name, path)) = pending.pop_front()
            {
                Self::enqueue(&mut scans, &mut indices, Arc::clone(&measure), index, name, path);
            }
        }

        match failure {
            Some((_, error)) => Err(error),
            None => Ok(sizes),
        }
    }

    fn enqueue<F>(
        scans: &mut JoinSet<(usize, String, std::io::Result<u64>)>,
        indices: &mut HashMap<tokio::task::Id, usize>,
        measure: Arc<F>,
        index: usize,
        name: String,
        path: PathBuf,
    ) where
        F: Fn(PathBuf) -> std::io::Result<u64> + Send + Sync + 'static,
    {
        let task = scans.spawn_blocking(move || (index, name, measure(path)));
        indices.insert(task.id(), index);
    }

    fn retain_failure(failure: &mut Option<(usize, Error)>, index: usize, error: Error) {
        if failure.as_ref().is_none_or(|(earliest, _)| index < *earliest) {
            *failure = Some((index, error));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SCAN_LIMIT, Volumes};
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
    async fn bind_failure() {
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
        let task = tokio::spawn(Volumes::scan(
            VecDeque::from([
                (0, "failure".into(), PathBuf::from("failure")),
                (1, "blocked".into(), PathBuf::from("blocked")),
            ]),
            move |path| {
                if path == std::path::Path::new("failure") {
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failure_order() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (first_started, first_waiting) = tokio::sync::oneshot::channel();
        let (second_finished, second_waiting) = tokio::sync::oneshot::channel();
        let first_started = Arc::new(Mutex::new(Some(first_started)));
        let second_finished = Arc::new(Mutex::new(Some(second_finished)));
        let task_gate = Arc::clone(&gate);
        let task = tokio::spawn(Volumes::scan(
            VecDeque::from([
                (0, "first".into(), PathBuf::from("first")),
                (1, "second".into(), PathBuf::from("second")),
            ]),
            move |path| {
                if path == std::path::Path::new("second") {
                    if let Some(finished) = second_finished.lock().unwrap().take() {
                        let _ = finished.send(());
                    }
                    return Err(std::io::Error::other("second"));
                }
                if let Some(started) = first_started.lock().unwrap().take() {
                    let _ = started.send(());
                }
                let (lock, wake) = &*task_gate;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
                Err(std::io::Error::other("first"))
            },
        ));
        first_waiting.await.unwrap();
        second_waiting.await.unwrap();
        {
            let (lock, wake) = &*gate;
            *lock.lock().unwrap() = true;
            wake.notify_all();
        }
        let Error::Io(error) = task.await.unwrap().unwrap_err() else {
            panic!("expected filesystem failure");
        };
        assert_eq!(error.to_string(), "first");
    }
}
