//! `docker` CLI helpers: locate the binary (the app bundle's PATH is minimal) and read/switch the
//! active docker context so the CLI can point at our daemon.

use std::path::PathBuf;

/// Absolute path to the `docker` CLI. A macOS app launched from Finder/Dock/launchd inherits a
/// minimal PATH (`/usr/bin:/bin:/usr/sbin:/sbin`) that excludes Homebrew (`/opt/homebrew/bin`),
/// `/usr/local/bin`, and Docker Desktop (`~/.docker/bin`) — so a bare `Command::new("docker")` fails
/// even when docker is installed and on the user's *shell* PATH (which is why it works in a terminal
/// but the app "can't see it"). Search the well-known install locations; fall back to the bare name
/// (PATH) for terminal/dev launches.
pub fn bin() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    for c in [
        "/opt/homebrew/bin/docker".to_string(),
        "/usr/local/bin/docker".to_string(),
        format!("{home}/.docker/bin/docker"),
        "/Applications/Docker.app/Contents/Resources/bin/docker".to_string(),
        "/usr/bin/docker".to_string(),
    ] {
        if std::path::Path::new(&c).exists() {
            return c.into();
        }
    }
    "docker".into()
}

/// The active `docker` context name, or `None` if the docker CLI isn't installed / errored.
pub(crate) async fn docker_context() -> Option<String> {
    let out = tokio::process::Command::new(bin())
        .args(["context", "show"])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// All selectable docker contexts (`docker context ls`), always including `hl` so the user can
/// pick our daemon even before the context exists.
pub(crate) async fn docker_contexts() -> Vec<String> {
    let out = tokio::process::Command::new(bin())
        .args(["context", "ls", "--format", "{{.Name}}"])
        .output()
        .await;
    let mut list: Vec<String> = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => vec![],
    };
    if !list.iter().any(|c| c == "dd") {
        list.push("dd".to_string());
    }
    list
}

/// Switch the `docker` CLI to context `name` (creating the `hl` context first if needed).
pub(crate) async fn set_context(name: &str, socket: &std::path::Path) {
    use tokio::process::Command;
    if name == "dd" {
        let host = format!("host=unix://{}", socket.display());
        let _ = Command::new(bin())
            .args(["context", "create", "dd", "--docker", &host])
            .output()
            .await;
    }
    let _ = Command::new(bin())
        .args(["context", "use", name])
        .output()
        .await;
}
