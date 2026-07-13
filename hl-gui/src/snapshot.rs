//! Off-thread daemon snapshots: the `Snapshot` type and the fetchers (state, logs, shells) that
//! run away from the UI thread and deliver their results back as `Cmd`s.

use crate::docker;
use crate::{AppModel, Cmd};
use hl_client::{Client, Container, DiskUsage, Image, Network, SystemInfo, Volume};
use relm4::prelude::ComponentSender;
use std::path::PathBuf;

/// A full snapshot of daemon state, fetched off the UI thread.
#[derive(Debug, Default)]
pub(crate) struct Snapshot {
    pub(crate) connected: bool,
    pub(crate) containers: Vec<Container>,
    pub(crate) images: Vec<Image>,
    pub(crate) networks: Vec<Network>,
    pub(crate) volumes: Vec<Volume>,
    /// Engine info + disk usage for the System view (None until first fetched).
    pub(crate) sys: Option<SystemInfo>,
    pub(crate) df: Option<DiskUsage>,
    /// Tail of the daemon's own log file (what the daemon is logging).
    pub(crate) daemon_log: String,
    /// Active `docker` context name, or `None` if the docker CLI isn't installed.
    pub(crate) docker_context: Option<String>,
    /// All selectable `docker` contexts (always includes `dd`).
    pub(crate) docker_contexts: Vec<String>,
}

/// Fetch a container's logs off-thread and deliver them as `Cmd::Logs`.
pub(crate) fn fetch_logs(sender: &ComponentSender<AppModel>, socket: PathBuf, id: String) {
    sender.oneshot_command(async move {
        let text = match Client::new(&socket).container_logs(&id).await {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => format!("could not fetch logs: {e}"),
        };
        Cmd::Logs(id, text)
    });
}

/// Probe which shells exist in a container (off-thread) → `Cmd::Shells`. Uses the container's own
/// `/bin/sh` + `command -v`; returns the basenames it finds, preference-ordered.
pub(crate) fn fetch_shells(sender: &ComponentSender<AppModel>, socket: PathBuf, id: String) {
    sender.oneshot_command(async move {
        let host = format!("unix://{}", socket.display());
        let docker = docker::bin();
        let out = std::process::Command::new(docker)
            .args([
                "--host",
                &host,
                "exec",
                &id,
                "/bin/sh",
                "-c",
                "command -v zsh bash ash dash sh busybox",
            ])
            .output();
        let mut shells: Vec<String> = Vec::new();
        if let Ok(o) = out {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                if let Some(name) = line.trim().rsplit('/').next() {
                    let name = name.trim();
                    if !name.is_empty() && !shells.iter().any(|s| s == name) {
                        shells.push(name.to_string());
                    }
                }
            }
        }
        let pref = ["zsh", "bash", "ash", "dash", "sh", "busybox"];
        shells.sort_by_key(|s| pref.iter().position(|p| p == s).unwrap_or(99));
        Cmd::Shells(id, shells)
    });
}

/// Keep only the last `n` lines of `text`.
pub(crate) fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Fetch a full snapshot. A failed `ping` short-circuits to a disconnected snapshot, but we still
/// report the active docker context (independent of whether our daemon is up).
pub(crate) async fn fetch(c: &Client) -> Snapshot {
    let docker_context = docker::docker_context().await;
    let docker_contexts = if docker_context.is_some() {
        docker::docker_contexts().await
    } else {
        vec![]
    };
    if c.ping().await.is_err() {
        return Snapshot {
            docker_context,
            docker_contexts,
            ..Snapshot::default()
        };
    }
    // Sort every list newest-first (descending). Containers/images carry a unix `created`; networks
    // and volumes carry an ISO-8601 `created_at` (lexicographic order == chronological).
    let mut containers = c.list_containers().await.unwrap_or_default();
    containers.sort_by(|a, b| b.created.cmp(&a.created));
    let mut images = c.list_images().await.unwrap_or_default();
    images.sort_by(|a, b| b.created.cmp(&a.created));
    let mut networks = c.list_networks().await.unwrap_or_default();
    networks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let mut volumes = c.list_volumes().await.unwrap_or_default();
    volumes.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Snapshot {
        connected: true,
        containers,
        images,
        networks,
        volumes,
        sys: c.system().await.ok(),
        df: c.disk_usage().await.ok(),
        daemon_log: read_daemon_log(),
        docker_context,
        docker_contexts,
    }
}

/// The daemon's own log: `$DD_DAEMON_LOG`, else `~/Library/Logs/dd/daemon.err.log`. Returns the last
/// ~400 lines so the System view shows what the daemon is logging without unbounded growth.
fn read_daemon_log() -> String {
    let path = std::env::var("DD_DAEMON_LOG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join("Library/Logs/dd/daemon.err.log")
        });
    match std::fs::read_to_string(&path) {
        Ok(s) => last_lines(&s, 400),
        Err(_) => String::new(),
    }
}
