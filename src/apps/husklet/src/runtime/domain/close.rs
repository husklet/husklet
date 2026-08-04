use std::io;
use std::path::{Path, PathBuf};

pub(super) struct ResultFile(PathBuf);

impl ResultFile {
    pub(super) fn new(runtime: &Path) -> Self {
        Self(runtime.join("close.result"))
    }

    pub(super) fn clear(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.0) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(super) fn publish(&self, result: &io::Result<()>) -> io::Result<()> {
        let value = match result {
            Ok(()) => "ok".to_owned(),
            Err(error) => format!("error\n{error}"),
        };
        hl_fs::File::from(self.0.clone()).replace(value)
    }

    pub(super) fn wait(&self, socket: &Path, timeout: std::time::Duration) -> io::Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match std::fs::read_to_string(&self.0) {
                Ok(value) if value.trim() == "ok" => return Ok(()),
                Ok(value) => {
                    let message = value.strip_prefix("error\n").unwrap_or(value.as_str()).trim();
                    return Err(io::Error::other(message.to_owned()));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            match std::os::unix::net::UnixStream::connect(socket) {
                Ok(_) => {}
                Err(error) if crate::runtime::process::Peer::offline(&error) => {
                    return Err(io::Error::other(
                        "workspace domain stopped without publishing its close result",
                    ));
                }
                Err(error) => return Err(error),
            }
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "workspace did not finish the close request",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ResultFile;

    #[test]
    fn result_file_preserves_a_checkpoint_rejection() {
        let root = tempfile::tempdir().unwrap();
        let result = ResultFile::new(root.path());
        result
            .publish(&Err(std::io::Error::other("terminal is not checkpointable")))
            .unwrap();
        let value = std::fs::read_to_string(root.path().join("close.result")).unwrap();
        assert_eq!(value, "error\nterminal is not checkpointable");
        result.clear().unwrap();
        assert!(!root.path().join("close.result").exists());
    }
}
