use std::path::PathBuf;

use clap::Parser;
use hl_container::{Config, Containers};
use hl_daemon::{Daemon, ProcessSample, ProcessSampler, Release};
use hl_images::Images;
use hl_images::Platform;
use hl_images::remote::{Auth, Registry};

#[derive(Clone, Copy)]
enum Disposition {
    Kill,
    Checkpoint,
}

struct Shutdown {
    cleanup: Containers,
    completed: std::sync::Arc<std::sync::Mutex<Option<hl_container::Result<()>>>>,
    response: PathBuf,
}

struct HostProcesses;

impl ProcessSampler for HostProcesses {
    fn sample(&self, process_id: u64) -> ProcessSample {
        let output = std::process::Command::new("ps")
            .args(["-o", "rss=,time=", "-p", &process_id.to_string()])
            .output();
        let Ok(output) = output else {
            return ProcessSample::default();
        };
        if !output.status.success() {
            return ProcessSample::default();
        }
        let output = String::from_utf8_lossy(&output.stdout);
        let mut fields = output.split_whitespace();
        ProcessSample {
            memory: fields
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_default()
                .saturating_mul(1024),
            cpu_seconds: fields.next().map_or(0, cpu_seconds),
        }
    }
}

fn cpu_seconds(value: &str) -> u64 {
    let (days, value) = value.split_once('-').map_or((0, value), |(days, value)| {
        (days.parse::<u64>().unwrap_or_default(), value)
    });
    value
        .split(':')
        .fold(0_u64, |total, value| {
            total
                .saturating_mul(60)
                .saturating_add(value.split('.').next().unwrap_or("0").parse().unwrap_or(0))
        })
        .saturating_add(days.saturating_mul(86_400))
}

#[derive(Debug, Parser)]
#[command(disable_version_flag = true)]
struct Arguments {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    socket: PathBuf,
    #[arg(long, requires = "external_images")]
    images: Option<PathBuf>,
    #[arg(long, requires = "images")]
    external_images: Option<PathBuf>,
}

impl Shutdown {
    #[cfg(unix)]
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

    #[cfg(not(unix))]
    async fn wait() -> Disposition {
        let _ = tokio::signal::ctrl_c().await;
        Disposition::Kill
    }

    async fn run(self) {
        loop {
            let disposition = Self::wait().await;
            let stopped = match disposition {
                Disposition::Kill => self.cleanup.shutdown(std::time::Duration::from_secs(5)).await,
                Disposition::Checkpoint => self.cleanup.checkpoint_all(std::time::Duration::from_secs(30)).await,
            };
            if matches!(disposition, Disposition::Checkpoint) {
                self.record_checkpoint(&stopped).await;
            }
            if matches!(disposition, Disposition::Checkpoint) && stopped.is_err() {
                continue;
            }
            if let Ok(mut result) = self.completed.lock() {
                *result = Some(stopped);
            }
            break;
        }
    }

    async fn record_checkpoint(&self, stopped: &hl_container::Result<()>) {
        match stopped {
            Ok(()) => {
                let _ = tokio::fs::remove_file(&self.response).await;
            }
            Err(error) => {
                let _ = tokio::fs::write(&self.response, error.to_string()).await;
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let logging = hl_log::EnvironmentConfig::parse(hl_log::Config::default(), std::env::vars());
    for warning in logging.warnings() {
        eprintln!("hl-daemon: {warning}");
    }
    logging.apply();
    if let Err(error) = Arguments::parse().run().await {
        eprintln!("hl-daemon: {error}");
        std::process::exit(1);
    }
}

impl Arguments {
    async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        let mut containers = Containers::builder(Config::new(&self.root));
        match (self.images, self.external_images) {
            (Some(local), Some(external)) => {
                containers = containers.images(Images::workspace(Images::open(local)?, Images::open(external)?));
            }
            (None, None) => {}
            _ => return Err("--images and --external-images must be supplied together".into()),
        }
        let containers = containers.build().await?;
        let failure = self.root.join("shutdown.error");
        match tokio::fs::remove_file(&failure).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let cleanup = containers.clone();
        let result = std::sync::Arc::new(std::sync::Mutex::new(None));
        let completed = std::sync::Arc::clone(&result);
        let shutdown = Shutdown {
            cleanup,
            completed,
            response: failure.clone(),
        };
        Daemon::new(containers)
            .platform(Platform::linux_arm64())
            .image_source(Registry::new(Auth::Anonymous))
            .process_sampler(HostProcesses)
            .release(Release::new(env!("CARGO_PKG_VERSION")))
            .server(self.socket)
            .serve_with_shutdown(shutdown.run())
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
}

#[cfg(test)]
mod tests {
    use super::{Arguments, cpu_seconds};
    use clap::{Parser, error::ErrorKind};
    use std::path::PathBuf;

    #[test]
    fn legacy_flags_preserve_the_installed_process_contract() {
        let arguments = Arguments::try_parse_from([
            "hl-daemon",
            "--root",
            "/data",
            "--socket",
            "/run/daemon.sock",
            "--images",
            "/images",
            "--external-images",
            "/external",
        ])
        .unwrap();

        assert_eq!(arguments.root, PathBuf::from("/data"));
        assert_eq!(arguments.socket, PathBuf::from("/run/daemon.sock"));
        assert_eq!(arguments.images, Some(PathBuf::from("/images")));
        assert_eq!(arguments.external_images, Some(PathBuf::from("/external")));
    }

    #[test]
    fn invalid_arguments_fail_before_runtime_construction() {
        assert_eq!(
            Arguments::try_parse_from(["hl-daemon", "--root"]).unwrap_err().kind(),
            ErrorKind::InvalidValue
        );
        assert_eq!(
            Arguments::try_parse_from([
                "hl-daemon",
                "--root",
                "/data",
                "--socket",
                "/run/daemon.sock",
                "--images",
                "/images",
            ])
            .unwrap_err()
            .kind(),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            Arguments::try_parse_from([
                "hl-daemon",
                "--root",
                "/data",
                "--socket",
                "/run/daemon.sock",
                "--unknown",
            ])
            .unwrap_err()
            .kind(),
            ErrorKind::UnknownArgument
        );
    }

    #[test]
    fn process_cpu_clock_formats() {
        assert_eq!(cpu_seconds("01:02"), 62);
        assert_eq!(cpu_seconds("1:02:03"), 3_723);
        assert_eq!(cpu_seconds("2-01:02:03.99"), 176_523);
    }
}
