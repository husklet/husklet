#[derive(Clone, Default)]
pub struct Environment(Vec<(String, String)>);

impl Environment {
    pub fn capture() -> Self {
        let keys = [
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
            "HL_ENGINE_FS_TRACE",
        ];
        let mut values: Vec<(String, String)> = keys
            .into_iter()
            .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_owned(), value)))
            .collect();
        values.push(("PATH".to_owned(), Self::path()));
        Self(values)
    }

    pub fn apply(&self, command: &mut std::process::Command) {
        command.env_clear().env("TERM", "xterm-256color");
        command.envs(self.0.iter().map(|(key, value)| (key, value)));
    }

    pub fn terminal(&self) -> Vec<String> {
        let mut values = vec!["TERM=xterm-256color".to_owned()];
        values.extend(self.0.iter().map(|(key, value)| format!("{key}={value}")));
        values
    }

    fn path() -> String {
        Self::path_from(std::env::var_os("PATH"), std::env::var_os("HOME"))
    }

    fn path_from(path: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>) -> String {
        let mut paths: Vec<std::path::PathBuf> = path
            .as_deref()
            .map(std::env::split_paths)
            .into_iter()
            .flatten()
            .collect();
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
            Some("/custom/first:/usr/bin:/custom/last:/usr/bin".into()),
            Some("/home/test".into()),
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
}
