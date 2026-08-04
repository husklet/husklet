//! Daemon spawning and socket plumbing shared by integration tests.

use hl_container::{Config, Containers};
use std::{
    env,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::{Child, Command},
    time::{sleep, timeout},
};

pub(crate) const TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) async fn containers_for(root: &Path) -> Result<Containers, hl_container::Error> {
    Containers::builder(Config::new(root.join("state")))
        .build()
        .await
}

pub(crate) fn spawn_daemon(
    state: &Path,
    socket: &Path,
) -> Result<Child, Box<dyn std::error::Error>> {
    Ok(Command::new(daemon_binary()?)
        .arg("--root")
        .arg(state)
        .arg("--socket")
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?)
}

pub(crate) fn daemon_binary() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = env::var_os("HL_DAEMON_BIN").map(PathBuf::from) {
        return Ok(path);
    }
    if let Some(path) = option_env!("CARGO_BIN_EXE_hl-daemon").map(PathBuf::from) {
        if path.is_file() {
            return Ok(path);
        }
    }
    let executable = env::current_exe()?;
    let parent = executable
        .parent()
        .ok_or("scenario executable has no parent directory")?;
    let target = if parent.file_name().and_then(|name| name.to_str()) == Some("deps") {
        parent
            .parent()
            .ok_or("scenario executable is not under target/{profile}/deps")?
    } else {
        parent
    };
    let path = target.join("hl-daemon");
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "{} is missing; run `cargo build -p hl-daemon --bin hl-daemon` first or set HL_DAEMON_BIN",
            path.display()
        )
        .into())
    }
}

pub(crate) async fn wait_for_socket(
    child: &mut Child,
    socket: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    timeout(TIMEOUT, async {
        loop {
            if UnixStream::connect(socket).await.is_ok() {
                return Ok(());
            }
            if let Some(status) = child.try_wait()? {
                return Err(format!("daemon exited before binding its socket: {status}").into());
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| "daemon socket startup timed out")?
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

pub(crate) async fn raw_http(
    socket: &Path,
    request: &[u8],
) -> Result<String, Box<dyn std::error::Error>> {
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
