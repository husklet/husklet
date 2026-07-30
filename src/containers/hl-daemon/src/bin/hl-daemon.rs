use std::path::PathBuf;

use hl_container::{Config, Containers};
use hl_daemon::{Daemon, Release};
use hl_images::Images;

#[derive(Clone, Copy)]
enum Disposition {
    Kill,
    Checkpoint,
}

struct Shutdown;

impl Shutdown {
    async fn wait() -> Disposition {
        let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        let hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup());
        if let (Ok(mut terminate), Ok(mut hangup)) = (terminate, hangup) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => Disposition::Kill,
                _ = terminate.recv() => Disposition::Kill,
                _ = hangup.recv() => Disposition::Checkpoint,
            }
        } else {
            let _ = tokio::signal::ctrl_c().await;
            Disposition::Kill
        }
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("hl-daemon: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let mut root = None;
    let mut socket = None;
    let mut images = None;
    let mut external_images = None;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--root") => root = arguments.next().map(PathBuf::from),
            Some("--socket") => socket = arguments.next().map(PathBuf::from),
            Some("--images") => images = arguments.next().map(PathBuf::from),
            Some("--external-images") => external_images = arguments.next().map(PathBuf::from),
            Some("--help" | "-h") => {
                println!(
                    "usage: hl-daemon --root PATH --socket PATH [--images PATH --external-images PATH]"
                );
                return Ok(());
            }
            _ => return Err(format!("unknown argument {}", argument.to_string_lossy()).into()),
        }
    }
    let root = root.ok_or("--root is required")?;
    let socket = socket.ok_or("--socket is required")?;
    let mut containers = Containers::builder(Config::new(&root));
    match (images, external_images) {
        (Some(local), Some(external)) => {
            containers = containers.images(Images::workspace(
                Images::open(local)?,
                Images::open(external)?,
            ));
        }
        (None, None) => {}
        _ => return Err("--images and --external-images must be supplied together".into()),
    }
    let containers = containers.build().await?;
    let failure = root.join("shutdown.error");
    match tokio::fs::remove_file(&failure).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let cleanup = containers.clone();
    let result = std::sync::Arc::new(std::sync::Mutex::new(None));
    let completed = std::sync::Arc::clone(&result);
    let response = failure.clone();
    Daemon::new(containers)
        .release(Release::new(env!("CARGO_PKG_VERSION")))
        .server(socket)
        .serve_with_shutdown(async move {
            loop {
                let disposition = Shutdown::wait().await;
                let stopped = match disposition {
                    Disposition::Kill => cleanup.shutdown(std::time::Duration::from_secs(5)).await,
                    Disposition::Checkpoint => {
                        cleanup
                            .checkpoint_all(std::time::Duration::from_secs(30))
                            .await
                    }
                };
                if matches!(disposition, Disposition::Checkpoint) {
                    match &stopped {
                        Ok(()) => {
                            let _ = tokio::fs::remove_file(&response).await;
                        }
                        Err(error) => {
                            let _ = tokio::fs::write(&response, error.to_string()).await;
                            continue;
                        }
                    }
                }
                if let Ok(mut result) = completed.lock() {
                    *result = Some(stopped);
                }
                break;
            }
        })
        .await?;
    let stopped = result
        .lock()
        .map_err(|_| "daemon cleanup result lock is poisoned")?
        .take()
        .ok_or("daemon cleanup did not run")?;
    if let Err(error) = stopped {
        tokio::fs::write(&failure, error.to_string()).await?;
        return Err(error.into());
    }
    Ok(())
}
