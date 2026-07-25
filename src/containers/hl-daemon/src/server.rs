use std::future::Future;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hl_container::Containers;
use hl_images::remote::Source;
use hl_images::Platform;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::UnixListener;
use tower::Service;

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
}

impl Server {
    pub(crate) fn new(
        socket: &Path,
        containers: Containers,
        platform: Platform,
        source: Arc<dyn Source>,
        events: Events,
        release: Release,
    ) -> Self {
        Self {
            socket: socket.to_path_buf(),
            containers,
            platform,
            source,
            events,
            release,
        }
    }

    /// Serve until Ctrl-C.
    ///
    /// # Errors
    /// Returns socket setup, accept, signal, or filesystem cleanup failures.
    pub async fn serve(self) -> Result<()> {
        self.serve_with_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
    }

    /// Serve until `shutdown` resolves, then stop accepting and remove the owned socket.
    ///
    /// # Errors
    /// Returns socket setup, accept, connection-drain, or filesystem cleanup failures.
    pub async fn serve_with_shutdown<F>(self, shutdown: F) -> Result<()>
    where
        F: Future<Output = ()> + Send,
    {
        let _span = hl_log::hl_span!(hl_log::tag::DAEMON, "serve");
        let listener = self.bind().await?;
        hl_log::hl_info!(
            hl_log::tag::DAEMON,
            "listening socket={}",
            self.socket.display()
        );
        let guard = SocketGuard::new(self.socket.clone())?;
        let app = router(
            self.containers,
            self.platform,
            self.source,
            self.events,
            self.release,
        );
        let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
        let mut connections = tokio::task::JoinSet::new();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
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
        hl_log::hl_info!(hl_log::tag::DAEMON, "stopped");
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

struct SocketGuard {
    path: PathBuf,
    identity: (u64, u64),
}

impl SocketGuard {
    fn new(path: PathBuf) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(&path)?;
        Ok(Self {
            path,
            identity: (metadata.dev(), metadata.ino()),
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
    use super::SocketGuard;

    #[test]
    fn guard_removes_only_the_socket_identity_it_owns() {
        let temporary = tempfile::tempdir().unwrap();
        let owned = temporary.path().join("owned.sock");
        let listener = std::os::unix::net::UnixListener::bind(&owned).unwrap();
        let guard = SocketGuard::new(owned.clone()).unwrap();
        drop(listener);
        drop(guard);
        assert!(!owned.exists());

        let replaced = temporary.path().join("replaced.sock");
        let original = std::os::unix::net::UnixListener::bind(&replaced).unwrap();
        let guard = SocketGuard::new(replaced.clone()).unwrap();
        drop(original);
        std::fs::remove_file(&replaced).unwrap();
        let replacement = std::os::unix::net::UnixListener::bind(&replaced).unwrap();
        drop(guard);
        assert!(replaced.exists());
        drop(replacement);
    }
}
