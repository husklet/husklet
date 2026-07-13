//! Installing the bundled `dd` CLI as a no-root `~/.local/bin` symlink.

use std::path::PathBuf;

/// Symlink the bundled `dd` CLI into `~/.local/bin` (no root). Returns `(link path, already on
/// PATH)`. The onboarding window turns this into per-shell instructions.
pub(crate) fn install_cli() -> Result<(PathBuf, bool), String> {
    let cli = resolve_cli().ok_or("dd CLI binary not found in the app bundle")?;
    let name = cli.file_name().ok_or("bad CLI path")?;
    let home = std::env::var("HOME").map_err(|_| "no HOME".to_string())?;
    let bindir = PathBuf::from(&home).join(".local/bin");
    std::fs::create_dir_all(&bindir).map_err(|e| e.to_string())?;
    let link = bindir.join(name);
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&cli, &link).map_err(|e| e.to_string())?;

    let on_path = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|p| p == bindir.to_string_lossy());
    Ok((link, on_path))
}

/// Locate the bundled `dd` CLI: `$DD_CLI_BIN`, the app bundle, or a sibling of this binary (dev).
fn resolve_cli() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DD_CLI_BIN") {
        return Some(PathBuf::from(p));
    }
    let names = ["ddcli", "dd"]; // whichever the CLI is built as
                                 // Prefer the *installed* bundle so the symlink stays valid across relaunches and updates
                                 // (which replace /Applications/dd.app in place), not the dev copy we run from.
    for n in names {
        let p = PathBuf::from("/Applications/dd.app/Contents/Resources").join(n);
        if p.exists() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    if let Some(contents) = exe.parent().and_then(|p| p.parent()) {
        for n in names {
            let c = contents.join("Resources").join(n);
            if c.exists() {
                return Some(c);
            }
        }
    }
    let dir = exe.parent()?;
    names.iter().map(|n| dir.join(n)).find(|p| p.exists())
}
