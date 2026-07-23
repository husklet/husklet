//! Canonical workspace runtime locations.

use std::path::PathBuf;

/// `$HOME`, or `.` as a last resort.
pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(Into::into)
        .unwrap_or_else(|| ".".into())
}

/// `~/.hl` — state root (images, volumes, state.json, run/).
pub fn hl_root() -> PathBuf {
    home().join(".hl")
}

/// `~/.hl/run` — runtime dir holding the socket.
pub fn run_dir() -> PathBuf {
    hl_root().join("run")
}

/// `~/.hl/images` — image rootfs dirs (== `HL_IMAGES`).
pub fn images_dir() -> PathBuf {
    hl_root().join("images")
}

/// Guest driver artifacts shipped with the application, or the development staging tree.
pub fn drivers_dir() -> PathBuf {
    DriverDirectory::resolve()
}

struct DriverDirectory;

impl DriverDirectory {
    fn resolve() -> PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|executable| Self::bundled(&executable))
            .unwrap_or_else(hl_root)
    }

    fn bundled(executable: &std::path::Path) -> Option<PathBuf> {
        let macos = executable.parent()?;
        if macos.file_name()? != "MacOS" {
            return None;
        }
        let directory = macos.parent()?.join("Resources/drivers");
        directory.is_dir().then_some(directory)
    }
}

/// Workspace resource daemon shipped beside the application or in its macOS resource directory.
pub fn daemon_bin() -> PathBuf {
    DaemonBinary::resolve()
}

struct DaemonBinary;

impl DaemonBinary {
    fn resolve() -> PathBuf {
        if let Some(p) = std::env::var_os("HL_DAEMON_BIN") {
            return PathBuf::from(p);
        }
        if let Some(binary) = Self::bundle() {
            return binary;
        }
        if let Some(sibling) = Self::sibling() {
            return sibling;
        }
        hl_root().join("hl-daemon")
    }

    fn sibling() -> Option<PathBuf> {
        let executable = std::env::current_exe().ok()?;
        let sibling = executable.parent()?.join("hl-daemon");
        sibling.exists().then_some(sibling)
    }

    fn bundle() -> Option<PathBuf> {
        let executable = std::env::current_exe().ok()?;
        let macos = executable.parent()?;
        if macos.file_name()? != "MacOS" {
            return None;
        }
        let binary = macos.parent()?.join("Resources/hl-daemon");
        binary.exists().then_some(binary)
    }
}

#[cfg(test)]
mod tests {
    use super::DriverDirectory;
    use std::path::Path;

    #[test]
    fn bundled_drivers_are_resolved_from_the_application_contents() {
        let root = tempfile::tempdir().unwrap();
        let contents = root.path().join("Husklet.app/Contents");
        let executable = contents.join("MacOS/husklet");
        std::fs::create_dir_all(contents.join("Resources/drivers")).unwrap();
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();

        assert_eq!(
            DriverDirectory::bundled(&executable),
            Some(contents.join("Resources/drivers"))
        );
        assert_eq!(
            DriverDirectory::bundled(Path::new("/usr/bin/husklet")),
            None
        );
    }
}
