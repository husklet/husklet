use crate::AppConfig;

pub(crate) struct Home(std::path::PathBuf);

impl Home {
    pub(crate) fn current() -> Self {
        Self(AppConfig::get().home.clone())
    }

    pub(crate) fn root(&self) -> std::path::PathBuf {
        self.0.join(".hl")
    }

    pub(crate) fn workspaces_config(&self) -> std::path::PathBuf {
        self.root().join("workspaces.conf")
    }

    pub(crate) fn display(&self, path: &std::path::Path) -> String {
        let display = path.to_string_lossy().into_owned();
        if let Some(home) = self.0.to_str() {
            if let Some(rest) = display.strip_prefix(home) {
                return format!("~{rest}");
            }
        }
        display
    }
}
