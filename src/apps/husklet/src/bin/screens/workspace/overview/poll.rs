use super::*;

#[derive(Default)]
struct DaemonSocketCache {
    socket: Option<std::path::PathBuf>,
}

impl DaemonSocketCache {
    fn resolve_with(
        &mut self,
        resolve: impl FnOnce() -> Result<std::path::PathBuf, String>,
    ) -> Result<std::path::PathBuf, String> {
        if let Some(socket) = &self.socket {
            return Ok(socket.clone());
        }
        let socket = resolve()?;
        if !socket.as_os_str().is_empty() {
            self.socket = Some(socket.clone());
        }
        Ok(socket)
    }
}

/// Latest snapshot of the workspace daemon's resources (rows are pre-formatted cell strings).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Data {
    pub(super) containers: Vec<Vec<String>>,
    pub(super) images: Vec<Vec<String>>,
    pub(super) volumes: Vec<Vec<String>>,
    pub(super) networks: Vec<Vec<String>>,
    pub(super) processes: Vec<Vec<String>>,
    pub(super) resources_error: Option<String>,
    pub(super) processes_error: Option<String>,
}

impl Data {
    pub(super) fn loading() -> Self {
        Self {
            resources_error: Some("Loading workspace resources…".into()),
            processes_error: Some("Loading workspace processes…".into()),
            ..Self::poll()
        }
    }

    fn poll() -> Self {
        Self {
            containers: Vec::new(),
            images: Vec::new(),
            volumes: Vec::new(),
            networks: Vec::new(),
            processes: Vec::new(),
            resources_error: None,
            processes_error: None,
        }
    }

    fn daemon_socket(workspace: &str) -> Result<std::path::PathBuf, String> {
        match Hl::command(&["daemon", workspace]).output() {
            Ok(output) if output.status.success() => Ok(std::path::PathBuf::from(
                String::from_utf8_lossy(&output.stdout).trim().to_string(),
            )),
            Ok(output) => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn merge_resources(&mut self, socket: &Result<std::path::PathBuf, String>) {
        let socket = match socket {
            Ok(socket) if !socket.as_os_str().is_empty() => socket,
            Ok(_) => {
                self.resources_error = Some("workspace daemon returned no socket".into());
                return;
            }
            Err(error) => {
                self.resources_error = Some(if error.is_empty() {
                    "workspace daemon unavailable".into()
                } else {
                    error.clone()
                });
                return;
            }
        };
        match WorkspaceResources::new(socket).read() {
            Ok(resources) => {
                self.containers = resources.containers;
                self.images = resources.images;
                self.volumes = resources.volumes;
                self.networks = resources.networks;
            }
            Err(error) => {
                self.resources_error = Some(format!("workspace resource query failed: {error}"));
            }
        }
    }

    fn merge_processes(&mut self, workspace: &str, shell: &str) {
        match WorkspaceProcesses::new(workspace, shell).read() {
            Ok(processes) => self.processes = processes,
            Err(error) => {
                self.processes_error = Some(format!("workspace process query failed: {error}"));
            }
        }
    }

    fn poll_forever(workspace: &str, shell: &str, data: &std::sync::Mutex<Self>, control: &PollControl) {
        let mut socket_cache = DaemonSocketCache::default();
        while control.running() {
            let mut snapshot = Self::poll();
            let socket = socket_cache.resolve_with(|| Self::daemon_socket(workspace));
            snapshot.merge_resources(&socket);
            snapshot.merge_processes(workspace, shell);
            // A management extension may have taken ownership while an
            // already-started query was finishing. Do not publish that stale
            // fallback snapshot, and do no further daemon work until resumed.
            if control.is_running() {
                *data.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = snapshot;
            }
            if !control.delay(std::time::Duration::from_secs(2)) {
                return;
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PollState {
    Running,
    Suspended,
    Stopped,
}

struct PollControl {
    state: std::sync::Mutex<PollState>,
    changed: std::sync::Condvar,
}

impl PollControl {
    fn suspended() -> Self {
        Self {
            state: std::sync::Mutex::new(PollState::Suspended),
            changed: std::sync::Condvar::new(),
        }
    }

    fn set(&self, state: PollState) {
        *self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = state;
        self.changed.notify_all();
    }

    fn is_running(&self) -> bool {
        *self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner) == PollState::Running
    }

    fn running(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        while *state == PollState::Suspended {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *state == PollState::Running
    }

    fn delay(&self, duration: std::time::Duration) -> bool {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if *state != PollState::Running {
            return *state != PollState::Stopped;
        }
        let (state, _) = self
            .changed
            .wait_timeout(state, duration)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *state != PollState::Stopped
    }
}

/// Handle for the fallback poller. It starts suspended so discovering an
/// installed management extension never races an unnecessary first query.
pub(super) struct OverviewPoller {
    control: std::sync::Arc<PollControl>,
}

impl OverviewPoller {
    pub(super) fn resume(&self) {
        self.control.set(PollState::Running);
    }

    pub(super) fn suspend(&self) {
        self.control.set(PollState::Suspended);
    }

    pub(super) fn stop(&self) {
        self.control.set(PollState::Stopped);
    }
}

/// Ensures the workspace daemon, then polls it over its Unix socket every two seconds.
pub(super) fn spawn_overview_poller(
    workspace: String,
    shell: String,
    data: std::sync::Arc<std::sync::Mutex<Data>>,
) -> OverviewPoller {
    let control = std::sync::Arc::new(PollControl::suspended());
    let worker_control = std::sync::Arc::clone(&control);
    std::thread::spawn(move || Data::poll_forever(&workspace, &shell, &data, &worker_control));
    OverviewPoller { control }
}

#[cfg(test)]
mod tests {
    use super::{DaemonSocketCache, Data, PollControl, PollState};

    #[test]
    fn initial_overview_never_claims_backend_results_are_empty() {
        let loading = Data::loading();
        assert!(loading.resources_error.as_deref().unwrap().contains("Loading"));
        assert!(loading.processes_error.as_deref().unwrap().contains("Loading"));

        let poll = Data::poll();
        assert_eq!(poll.resources_error, None);
        assert_eq!(poll.processes_error, None);
        assert_ne!(loading, poll);
    }

    #[test]
    fn daemon_socket_resolution_retries_until_a_nonempty_success() {
        let mut cache = DaemonSocketCache::default();
        let mut attempts = 0;

        assert_eq!(
            cache.resolve_with(|| {
                attempts += 1;
                Err("daemon starting".into())
            }),
            Err("daemon starting".into())
        );
        assert_eq!(
            cache.resolve_with(|| {
                attempts += 1;
                Ok(std::path::PathBuf::new())
            }),
            Ok(std::path::PathBuf::new())
        );

        let socket = std::path::PathBuf::from("/tmp/husklet-daemon.sock");
        assert_eq!(
            cache.resolve_with(|| {
                attempts += 1;
                Ok(socket.clone())
            }),
            Ok(socket.clone())
        );
        assert_eq!(
            cache.resolve_with(|| panic!("successful socket resolution must be cached")),
            Ok(socket)
        );
        assert_eq!(attempts, 3);
    }

    #[test]
    fn suspended_poller_does_no_work_until_resumed_and_stops_promptly() {
        let control = std::sync::Arc::new(PollControl::suspended());
        let cycles = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker_control = std::sync::Arc::clone(&control);
        let worker_cycles = std::sync::Arc::clone(&cycles);
        let worker = std::thread::spawn(move || {
            while worker_control.running() {
                worker_cycles.fetch_add(1, std::sync::atomic::Ordering::Release);
                if !worker_control.delay(std::time::Duration::from_millis(10)) {
                    break;
                }
            }
        });

        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(cycles.load(std::sync::atomic::Ordering::Acquire), 0);
        control.set(PollState::Running);
        while cycles.load(std::sync::atomic::Ordering::Acquire) == 0 {
            std::thread::yield_now();
        }
        control.set(PollState::Suspended);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let paused = cycles.load(std::sync::atomic::Ordering::Acquire);
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(cycles.load(std::sync::atomic::Ordering::Acquire), paused);
        control.set(PollState::Running);
        while cycles.load(std::sync::atomic::Ordering::Acquire) == paused {
            std::thread::yield_now();
        }
        control.set(PollState::Stopped);
        worker.join().expect("controlled poll worker exits");
    }
}
