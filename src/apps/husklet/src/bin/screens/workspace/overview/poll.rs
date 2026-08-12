use super::*;

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

    fn poll_forever(workspace: &str, shell: &str, data: &std::sync::Mutex<Self>, stop: &std::sync::atomic::AtomicBool) {
        let socket = Self::daemon_socket(workspace);
        loop {
            if stop.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }

            let mut snapshot = Self::poll();
            snapshot.merge_resources(&socket);
            snapshot.merge_processes(workspace, shell);
            *data.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = snapshot;
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    }
}

/// Ensures the workspace daemon, then polls it over its Unix socket every two seconds.
pub(super) fn spawn_overview_poller(
    workspace: String,
    shell: String,
    data: std::sync::Arc<std::sync::Mutex<Data>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    std::thread::spawn(move || Data::poll_forever(&workspace, &shell, &data, &stop));
}

#[cfg(test)]
mod tests {
    use super::Data;

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
}
