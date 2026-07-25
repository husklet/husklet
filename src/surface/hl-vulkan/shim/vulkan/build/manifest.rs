use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Manifest {
    path: PathBuf,
    text: String,
    kinds: HashMap<String, String>,
}

impl Manifest {
    pub fn load() -> Self {
        let path =
            PathBuf::from(Self::env("CARGO_MANIFEST_DIR")).join("registry/vk_commands.manifest");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let mut kinds = HashMap::new();

        for line in text.lines() {
            let line = line.trim_end();
            if !line.starts_with("T\t") {
                continue;
            }
            let mut fields = line.split('\t');
            fields.next();
            let name = fields.next().unwrap_or("");
            let kind = fields.next().unwrap_or("");
            kinds.insert(name.to_string(), kind.to_string());
        }

        Self { path, text, kinds }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn kinds(&self) -> &HashMap<String, String> {
        &self.kinds
    }

    pub fn output() -> PathBuf {
        PathBuf::from(Self::env("OUT_DIR")).join("generated_entrypoints.rs")
    }

    fn env(key: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| panic!("env {key} not set"))
    }
}
