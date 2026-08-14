//! Hosts an extension that runs as a real process.
//!
//! The in-process modes prove the protocol; this proves the product. A command
//! is started with nothing but a socket path in its environment, exactly as a
//! sidecar container is, and whatever it draws is rendered. Nothing about the
//! extension's language reaches this file.

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::Fault;

/// Where the host tells an extension to find its socket.
const SOCKET: &str = "HUSKLET_EXTENSION_SOCKET";
/// How long an extension may run before it is stopped regardless.
const LIFETIME: Duration = Duration::from_secs(120);
/// Output kept from the extension, so a chatty one cannot fill the disk.
const OUTPUT: u64 = 64 * 1024;

/// A listening socket and the process invited to connect to it.
pub struct Guest {
    directory: PathBuf,
    listener: UnixListener,
    cancelled: Arc<AtomicBool>,
    running: Option<std::thread::JoinHandle<()>>,
}

impl Guest {
    /// Binds a socket and starts `command` pointed at it.
    ///
    /// # Errors
    /// Returns why the socket could not be bound or the command not described.
    pub fn invite(command: &str) -> Result<Self, Fault> {
        let directory = std::env::temp_dir().join(format!("husklet-storybook-{}", std::process::id()));
        std::fs::create_dir_all(&directory).map_err(|error| Fault::Socket(error.to_string()))?;
        let socket = directory.join("extension.sock");
        // A stale socket from an earlier run would refuse the bind.
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).map_err(|error| Fault::Socket(error.to_string()))?;

        let described = Self::describe(command, &socket)?;
        let logs = directory.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&cancelled);
        let running = std::thread::spawn(move || {
            let directory = logs;
            let capture = hl_process::Capture {
                stdout: directory.join("stdout.log"),
                stderr: directory.join("stderr.log"),
                stdout_limit: OUTPUT,
                stderr_limit: OUTPUT,
            };
            match hl_process::run(&described, &capture, LIFETIME, &stopping) {
                Ok(outcome) => eprintln!("[storybook] the extension ended: {outcome:?}"),
                Err(error) => eprintln!("[storybook] the extension could not run: {error}"),
            }
        });

        Ok(Self {
            directory,
            listener,
            cancelled,
            running: Some(running),
        })
    }

    /// Waits for the extension to connect.
    ///
    /// # Errors
    /// Returns why no connection arrived.
    pub fn accept(&self) -> Result<UnixStream, Fault> {
        let (stream, _) = self.listener.accept().map_err(|error| Fault::Socket(error.to_string()))?;
        Ok(stream)
    }

    /// The command as typed data: the program, its arguments, and an
    /// environment holding the socket and nothing else about this host —
    /// which is the whole environment a sidecar container receives.
    fn describe(command: &str, socket: &Path) -> Result<hl_process::Command, Fault> {
        let mut parts = command.split_whitespace();
        let program = parts.next().ok_or_else(|| Fault::Socket("no command given".into()))?;
        let mut described = hl_process::Command::new(program);
        described.args(parts);
        let entry = hl_process::EnvironmentEntry::new(SOCKET, socket.as_os_str().as_encoded_bytes())
            .map_err(|error| Fault::Socket(error.to_string()))?;
        described
            .exact_environment([entry])
            .map_err(|error| Fault::Socket(error.to_string()))?;
        Ok(described)
    }
}

impl Drop for Guest {
    fn drop(&mut self) {
        // The extension is ours to stop; leaving it running would hold the
        // socket open after the window it was drawing has gone.
        self.cancelled.store(true, Ordering::Release);
        if let Some(running) = self.running.take() {
            let _ = running.join();
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
