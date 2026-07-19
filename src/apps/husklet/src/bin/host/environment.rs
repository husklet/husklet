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
        let mut paths: Vec<std::path::PathBuf> = std::env::var_os("PATH")
            .map(|value| std::env::split_paths(&value).collect())
            .unwrap_or_default();
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(std::path::PathBuf::from(home).join(".local/bin"));
        }
        #[cfg(target_os = "macos")]
        paths.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(Into::into));
        paths.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(Into::into));
        paths.sort();
        paths.dedup();
        std::env::join_paths(paths)
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
}
