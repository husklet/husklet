use super::*;

/// Latest snapshot of the workspace daemon's resources (rows are pre-formatted cell strings).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct OverviewData {
    pub(super) containers: Vec<Vec<String>>,
    pub(super) images: Vec<Vec<String>>,
    pub(super) volumes: Vec<Vec<String>>,
    pub(super) networks: Vec<Vec<String>>,
    pub(super) processes: Vec<Vec<String>>,
    pub(super) resources_error: Option<String>,
    pub(super) processes_error: Option<String>,
}

impl OverviewData {
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
}

/// Ensures the workspace daemon, then polls it over its Unix socket every two seconds.
pub(super) fn spawn_overview_poller(
    workspace: String,
    shell: String,
    data: std::sync::Arc<std::sync::Mutex<OverviewData>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    std::thread::spawn(move || {
        let socket = match Hl::command(&["daemon", &workspace]).output() {
            Ok(output) if output.status.success() => Ok(std::path::PathBuf::from(
                String::from_utf8_lossy(&output.stdout).trim().to_string(),
            )),
            Ok(output) => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
            Err(error) => Err(error.to_string()),
        };

        loop {
            if stop.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }

            let mut snapshot = OverviewData::poll();
            match &socket {
                Ok(socket) if !socket.as_os_str().is_empty() => {
                    match WorkspaceResources::new(socket).read() {
                        Ok(resources) => {
                            snapshot.containers = resources.containers;
                            snapshot.images = resources.images;
                            snapshot.volumes = resources.volumes;
                            snapshot.networks = resources.networks;
                        }
                        Err(error) => {
                            snapshot.resources_error =
                                Some(format!("workspace resource query failed: {error}"));
                        }
                    }
                }
                Ok(_) => {
                    snapshot.resources_error = Some("workspace daemon returned no socket".into());
                }
                Err(error) => {
                    snapshot.resources_error = Some(if error.is_empty() {
                        "workspace daemon unavailable".into()
                    } else {
                        error.clone()
                    });
                }
            }

            match WorkspaceProcesses::new(&workspace, &shell).read() {
                Ok(processes) => snapshot.processes = processes,
                Err(error) => {
                    snapshot.processes_error =
                        Some(format!("workspace process query failed: {error}"));
                }
            }

            *data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = snapshot;
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::OverviewData;

    #[test]
    fn initial_overview_never_claims_backend_results_are_empty() {
        let loading = OverviewData::loading();
        assert!(loading
            .resources_error
            .as_deref()
            .unwrap()
            .contains("Loading"));
        assert!(loading
            .processes_error
            .as_deref()
            .unwrap()
            .contains("Loading"));

        let poll = OverviewData::poll();
        assert_eq!(poll.resources_error, None);
        assert_eq!(poll.processes_error, None);
        assert_ne!(loading, poll);
    }
}
