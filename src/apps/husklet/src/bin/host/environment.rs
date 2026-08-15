#[derive(Clone, Default)]
pub struct Environment(Vec<(String, String)>);

const TERMINAL_CAPABILITIES: [(&str, &str); 2] = [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")];

impl Environment {
    pub fn capture() -> Self {
        let keys = vec![
            "HOME",
            "USER",
            "LOGNAME",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TMPDIR",
            "SSH_AUTH_SOCK",
            "HL_LOG",
            "HL_LOG_LEVEL",
            "HL_LOG_COUNTERS",
        ];
        // The GUI re-execs its current binary as a worker. Cargo supplies the development build's
        // `libhl_native_engine.so` through this path, so dropping it makes every Linux workspace
        // terminal exit 127 before the worker can start. Release bundles normally leave it absent.
        // macOS deliberately continues to exclude DYLD_* loader variables.
        #[cfg(target_os = "linux")]
        let keys = keys.into_iter().chain(["LD_LIBRARY_PATH"]);
        #[cfg(not(target_os = "linux"))]
        let keys = keys.into_iter();
        let mut values: Vec<(String, String)> = keys
            .into_iter()
            .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_owned(), value)))
            .collect();
        values.push(("PATH".to_owned(), Self::path()));
        Self(values)
    }

    pub fn apply(&self, command: &mut std::process::Command) {
        command.env_clear().envs(TERMINAL_CAPABILITIES);
        command.envs(self.0.iter().map(|(key, value)| (key, value)));
    }

    pub fn terminal(&self) -> Vec<String> {
        let mut values = TERMINAL_CAPABILITIES
            .map(|(key, value)| format!("{key}={value}"))
            .to_vec();
        values.extend(self.0.iter().map(|(key, value)| format!("{key}={value}")));
        values
    }

    fn path() -> String {
        Self::path_from(std::env::var_os("PATH").as_deref(), std::env::var_os("HOME").as_deref())
    }

    fn path_from(path: Option<&std::ffi::OsStr>, home: Option<&std::ffi::OsStr>) -> String {
        let mut paths: Vec<std::path::PathBuf> = path.map(std::env::split_paths).into_iter().flatten().collect();
        if let Some(home) = home {
            paths.push(std::path::PathBuf::from(home).join(".local/bin"));
        }
        #[cfg(target_os = "macos")]
        paths.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(Into::into));
        paths.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(Into::into));
        let mut seen = std::collections::HashSet::new();
        paths.retain(|path| seen.insert(path.clone()));
        std::env::join_paths(paths)
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::Environment;

    #[test]
    fn inherited_path_precedence_is_preserved_while_defaults_are_deduplicated() {
        let path = Environment::path_from(
            Some(std::ffi::OsStr::new("/custom/first:/usr/bin:/custom/last:/usr/bin")),
            Some(std::ffi::OsStr::new("/home/test")),
        );
        let paths: Vec<_> = std::env::split_paths(&path).collect();

        assert_eq!(paths[0], std::path::Path::new("/custom/first"));
        assert_eq!(paths[1], std::path::Path::new("/usr/bin"));
        assert_eq!(paths[2], std::path::Path::new("/custom/last"));
        assert_eq!(
            paths
                .iter()
                .filter(|path| path.as_path() == std::path::Path::new("/usr/bin"))
                .count(),
            1
        );
        assert!(paths.contains(&std::path::PathBuf::from("/home/test/.local/bin")));
    }

    #[test]
    fn terminal_environment_advertises_vte_truecolor_support() {
        assert_eq!(
            Environment::default().terminal(),
            ["TERM=xterm-256color", "COLORTERM=truecolor"]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cargo_launched_worker_reexec_preserves_native_library_path() {
        assert!(Environment::capture().0.iter().any(|(key, _)| key == "LD_LIBRARY_PATH"));
    }
}
