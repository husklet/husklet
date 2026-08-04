use crate::config::WorkspaceConfig;
use crate::paths;
use std::io;
use std::path::PathBuf;

pub(super) struct RestoreSummary {
    path: PathBuf,
}

impl RestoreSummary {
    pub(super) fn new(workspace: &WorkspaceConfig) -> Self {
        Self {
            path: workspace.storage_dir(&paths::hl_root()).join("state/restore.txt"),
        }
    }

    pub(super) fn publish(&self, failures: &[String]) -> io::Result<()> {
        if failures.is_empty() {
            return self.remove();
        }
        let summary = format!(
            "workspace restored with {} failure(s):\n- {}\n",
            failures.len(),
            failures.join("\n- ")
        );
        hl_fs::File::from(self.path.clone()).replace(summary)
    }

    pub(super) fn take(&self) -> io::Result<Option<String>> {
        match std::fs::read_to_string(&self.path) {
            Ok(summary) => {
                self.remove()?;
                Ok(Some(summary))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn remove(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}
