//! Daemon spawning and socket plumbing shared by integration tests.

use hl_container::{Config, Containers};
use std::{path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    time::{sleep, timeout},
};

pub(crate) const TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) async fn containers_for(root: &Path) -> Result<Containers, hl_container::Error> {
    Containers::builder(Config::new(root.join("state"))).build().await
}

pub(crate) async fn wait_for_path(socket: &Path) -> Result<(), Box<dyn std::error::Error>> {
    timeout(TIMEOUT, async {
        while !socket.exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| "embedded daemon socket startup timed out".into())
}

pub(crate) async fn raw_http(socket: &Path, request: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
    timeout(TIMEOUT, async {
        let mut stream = UnixStream::connect(socket).await?;
        stream.write_all(request).await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        if response.len() > 1024 * 1024 {
            return Err("HTTP error response exceeded one MiB".into());
        }
        String::from_utf8(response).map_err(Into::into)
    })
    .await
    .map_err(|_| "raw HTTP exchange timed out")?
}
