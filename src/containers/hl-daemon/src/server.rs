use std::future::Future;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hl_container::Containers;
use hl_images::Platform;
use hl_images::remote::Source;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::UnixListener;
use tower::Service;

use crate::ProcessSampler;
use crate::api::router;
use crate::daemon::Release;
use crate::events::Events;
use crate::{Error, Result};

/// Configured Docker HTTP server over a Unix socket.
pub struct Server {
    socket: PathBuf,
    containers: Containers,
    platform: Platform,
    source: Arc<dyn Source>,
    events: Events,
    release: Release,
    sampler: Arc<dyn ProcessSampler>,
    sandbox: hl_container::Sandbox,
}

impl Server {
    pub(crate) fn new(
        socket: &Path,
        containers: Containers,
        platform: Platform,
        source: Arc<dyn Source>,
        events: Events,
        release: Release,
        sampler: Arc<dyn ProcessSampler>,
        sandbox: hl_container::Sandbox,
    ) -> Self {
        Self {
            socket: socket.to_path_buf(),
            containers,
            platform,
            source,
            events,
            release,
            sampler,
            sandbox,
        }
    }

    /// Serve until `shutdown` resolves, then stop accepting and remove the owned socket.
    ///
    /// # Errors
    /// Returns socket setup, accept, connection-drain, or filesystem cleanup failures.
    pub async fn serve_with_shutdown<F>(self, shutdown: F) -> Result<()>
    where
        F: Future<Output = ()> + Send,
    {
        let socket = self.socket.clone();
        hl_log::hl_info!(hl_log::tag::DAEMON, "server starting socket={}", socket.display());
        hl_log::hl_event!(
            hl_log::tag::DAEMON,
            hl_log::Level::Info,
            "daemon.server.starting",
            socket = %socket.display()
        );
        let result = self.serve_loop(shutdown).await;
        match &result {
            Ok(()) => {
                hl_log::hl_info!(hl_log::tag::DAEMON, "server stopped socket={}", socket.display());
                hl_log::hl_event!(
                    hl_log::tag::DAEMON,
                    hl_log::Level::Info,
                    "daemon.server.stopped",
                    socket = %socket.display()
                );
            }
            Err(error) => {
                hl_log::hl_error!(
                    hl_log::tag::DAEMON,
                    "server failed socket={} reason={error}",
                    socket.display()
                );
                hl_log::hl_event!(
                    hl_log::tag::DAEMON,
                    hl_log::Level::Error,
                    "daemon.server.failed",
                    socket = %socket.display(),
                    reason = %error
                );
            }
        }
        result
    }

    async fn serve_loop<F>(self, shutdown: F) -> Result<()>
    where
        F: Future<Output = ()> + Send,
    {
        let _span = hl_log::hl_span!(hl_log::tag::DAEMON, "serve");
        let listener = self.bind().await?;
        hl_log::hl_info!(hl_log::tag::DAEMON, "listening socket={}", self.socket.display());
        hl_log::hl_event!(
            hl_log::tag::DAEMON,
            hl_log::Level::Info,
            "daemon.server.listening",
            socket = %self.socket.display()
        );
        let guard = SocketGuard::new(self.socket.clone(), &listener)?;
        let app = router(
            self.containers,
            self.platform,
            self.source,
            self.events,
            self.release,
            self.sampler,
            self.sandbox,
        );
        let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
        let mut connections = tokio::task::JoinSet::new();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => {
                    hl_log::hl_info!(hl_log::tag::DAEMON, "server stopping socket={}", self.socket.display());
                    hl_log::hl_event!(
                        hl_log::tag::DAEMON,
                        hl_log::Level::Info,
                        "daemon.server.stopping",
                        socket = %self.socket.display()
                    );
                    break;
                },
                accepted = listener.accept() => {
                    let (socket, _) = accepted?;
                    let service = app.clone().into_service();
                    let mut connection_shutdown = shutdown_receiver.clone();
                    connections.spawn(async move {
                        let service = hyper::service::service_fn(move |request| {
                            let mut service = service.clone();
                            async move { service.call(request).await }
                        });
                        let builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
                        let connection = builder.serve_connection_with_upgrades(TokioIo::new(socket), service);
                        tokio::pin!(connection);
                        tokio::select! {
                            _ = &mut connection => {}
                            changed = connection_shutdown.changed() => {
                                if changed.is_ok() { connection.as_mut().graceful_shutdown(); }
                                let _ = connection.await;
                            }
                        }
                    });
                }
            }
        }
        drop(listener);
        let _ = shutdown_sender.send(true);
        while connections.join_next().await.is_some() {}
        drop(guard);
        Ok(())
    }

    async fn bind(&self) -> Result<UnixListener> {
        let parent = self
            .socket
            .parent()
            .filter(|value| !value.as_os_str().is_empty())
            .ok_or(Error::SocketParent)?;
        tokio::fs::create_dir_all(parent).await?;
        tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).await?;
        match tokio::fs::symlink_metadata(&self.socket).await {
            Ok(metadata) if metadata.file_type().is_socket() => {
                tokio::fs::remove_file(&self.socket).await?;
            }
            Ok(_) => return Err(Error::OccupiedSocket(self.socket.clone())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(UnixListener::bind(&self.socket)?)
    }
}

/// Owns the pathname of the socket this server bound, and removes it at shutdown only while the
/// pathname still names that very socket.
///
/// `(device, inode)` on its own cannot decide that. A local filesystem reissues a freed inode
/// number immediately -- measured on ext4, unlink and rebind at the same pathname hand back the
/// identical number -- so the socket a successor daemon binds after this one stops listening reads
/// as this server's own, and the guard would remove a live socket out from under it. The guard
/// therefore keeps a descriptor on the socket it bound: an `AF_UNIX` socket holds its directory
/// entry's inode allocated for as long as any descriptor refers to it, so no later file can be
/// issued that number while the guard lives, and the comparison becomes exact.
///
/// The pathname is still read once after the bind rather than derived from the descriptor, because
/// the socket descriptor reports its own `sockfs` inode and not the filesystem entry's. A
/// replacement landing inside that window would be recorded as this server's own; closing it needs
/// a bind that reports the entry it created, which Linux does not offer.
struct SocketGuard {
    path: PathBuf,
    identity: (u64, u64),
    /// Holds `identity` unrepeatable for this guard's lifetime; never read.
    _bound: OwnedFd,
}

impl SocketGuard {
    fn new(path: PathBuf, bound: impl AsFd) -> Result<Self> {
        let bound = bound.as_fd().try_clone_to_owned()?;
        let metadata = std::fs::symlink_metadata(&path)?;
        Ok(Self {
            path,
            identity: (metadata.dev(), metadata.ino()),
            _bound: bound,
        })
    }

    fn owns_path(&self) -> bool {
        std::fs::symlink_metadata(&self.path)
            .map(|metadata| (metadata.dev(), metadata.ino()))
            .ok()
            == Some(self.identity)
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if self.owns_path() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::SocketGuard;

    static LOGGING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct Collector(Arc<Mutex<Vec<String>>>);

    impl hl_log::Sink for Collector {
        fn write_line(&self, line: &str) {
            self.0.lock().unwrap().push(line.to_owned());
        }
    }

    struct LoggingState(hl_log::Config);

    impl LoggingState {
        fn capture(records: &Arc<Mutex<Vec<String>>>) -> Self {
            let previous = hl_log::Config {
                logging: hl_log::Logging::global().tags(),
                level: hl_log::Logging::global().level(),
                profiling: hl_log::Profiling::global().tags(),
            };
            hl_log::Events::global().set(Box::new(Collector(Arc::clone(records))));
            hl_log::Config {
                logging: hl_log::tag::DAEMON.into(),
                level: hl_log::Level::Info,
                profiling: hl_log::Tags::NONE,
            }
            .apply();
            Self(previous)
        }
    }

    impl Drop for LoggingState {
        fn drop(&mut self) {
            self.0.apply();
            hl_log::Events::global().reset();
        }
    }

    async fn containers(root: &std::path::Path) -> hl_container::Containers {
        hl_container::Containers::builder(hl_container::Config::new(root))
            .build()
            .await
            .unwrap()
    }

    #[test]
    fn socket_guard_ownership() {
        use std::os::unix::fs::MetadataExt as _;

        let temporary = tempfile::tempdir().unwrap();
        let owned = temporary.path().join("owned.sock");
        let listener = std::os::unix::net::UnixListener::bind(&owned).unwrap();
        let guard = SocketGuard::new(owned.clone(), &listener).unwrap();
        drop(listener);
        drop(guard);
        assert!(!owned.exists());

        let replaced = temporary.path().join("replaced.sock");
        let original = std::os::unix::net::UnixListener::bind(&replaced).unwrap();
        let guard = SocketGuard::new(replaced.clone(), &original).unwrap();
        let bound = std::fs::symlink_metadata(&replaced).unwrap().ino();
        drop(original);
        std::fs::remove_file(&replaced).unwrap();
        let replacement = std::os::unix::net::UnixListener::bind(&replaced).unwrap();
        assert_ne!(
            std::fs::symlink_metadata(&replaced).unwrap().ino(),
            bound,
            "the guard must hold the inode it bound allocated, or its recorded identity is reissuable"
        );
        drop(guard);
        assert!(replaced.exists());
        drop(replacement);
    }

    #[tokio::test]
    async fn occupied_reports_failure() {
        let _serial = LOGGING.lock().await;
        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("occupied.sock");
        std::fs::write(&socket, b"not a socket").unwrap();
        let containers = containers(&temporary.path().join("containers")).await;
        let records = Arc::new(Mutex::new(Vec::new()));
        let _logging = LoggingState::capture(&records);

        let result = crate::Daemon::new(containers)
            .server(&socket)
            .serve_with_shutdown(std::future::pending())
            .await;

        assert!(matches!(result, Err(crate::Error::OccupiedSocket(path)) if path == socket));
        let records = records.lock().unwrap();
        assert!(
            records
                .iter()
                .any(|record| record.contains(r#""event":"daemon.server.starting""#))
        );
        assert!(
            records
                .iter()
                .any(|record| record.contains(r#""event":"daemon.server.failed""#))
        );
        assert!(!records.iter().any(|record| record.contains("daemon.server.listening")));
        assert!(!records.iter().any(|record| record.contains("daemon.server.stopped")));
    }

    #[tokio::test]
    async fn graceful_sequence() {
        let _serial = LOGGING.lock().await;
        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("daemon.sock");
        let containers = containers(&temporary.path().join("containers")).await;
        let records = Arc::new(Mutex::new(Vec::new()));
        let _logging = LoggingState::capture(&records);

        crate::Daemon::new(containers)
            .server(&socket)
            .serve_with_shutdown(async {})
            .await
            .unwrap();

        let records = records.lock().unwrap();
        let events = records
            .iter()
            .filter_map(|record| {
                ["starting", "listening", "stopping", "stopped"]
                    .into_iter()
                    .find(|event| record.contains(&format!(r#""event":"daemon.server.{event}""#)))
            })
            .collect::<Vec<_>>();
        assert_eq!(events, ["starting", "listening", "stopping", "stopped"]);
        assert!(!records.iter().any(|record| record.contains("daemon.server.failed")));
        assert!(!socket.exists());
    }
}
