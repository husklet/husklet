use std::{fs, path::Path};

use serde::Deserialize;

use crate::{LintError, Result};

/// Complete repository-owned policy for the generic analyzers.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Cargo dependency graph and architectural layer policy.
    #[serde(default)]
    pub dependency: DependencyPolicy,
}

impl Policy {
    /// Loads repository policy from TOML.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|error| LintError::io("read policy", path, error))?;
        toml::from_str(&text).map_err(|error| {
            LintError::io(
                "parse policy",
                path,
                std::io::Error::new(std::io::ErrorKind::InvalidData, error),
            )
        })
    }
}

/// Repository-owned policy consumed by the generic dependency analyzer.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyPolicy {
    /// Package names omitted from dependency analysis.
    #[serde(default)]
    pub ignored_packages: Vec<String>,
    /// Whether every local dependency must occur in `edges`.
    #[serde(default)]
    pub require_reviewed_edges: bool,
    /// Directory-to-layer classifications and permitted layer directions.
    #[serde(default)]
    pub layers: Vec<LayerPolicy>,
    /// Reviewed package dependency groups.
    #[serde(default)]
    pub edges: Vec<EdgePolicy>,
}

/// A generic architectural layer selected by a path component.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerPolicy {
    /// Stable layer name used in diagnostics.
    pub name: String,
    /// Path component immediately below `src` that selects this layer.
    pub directory: String,
    /// Layers that packages in this layer may depend upon.
    #[serde(default)]
    pub may_depend_on: Vec<String>,
}

/// One compact set of reviewed source-to-target package edges.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgePolicy {
    /// Source package names.
    pub sources: Vec<String>,
    /// Target package names.
    pub targets: Vec<String>,
    /// Dependency kinds accepted by this edge group.
    #[serde(default = "production_kinds")]
    pub kinds: Vec<DependencyKind>,
}

/// Cargo dependency table categories understood by the analyzer.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum DependencyKind {
    /// `[dependencies]`.
    Normal,
    /// `[dev-dependencies]`.
    Development,
    /// `[build-dependencies]`.
    Build,
}

fn production_kinds() -> Vec<DependencyKind> {
    vec![DependencyKind::Normal, DependencyKind::Build]
}

impl DependencyPolicy {
    pub(crate) fn layer(&self, directory: &str) -> Option<&LayerPolicy> {
        self.layers.iter().find(|layer| layer.directory == directory)
    }

    pub(crate) fn permits_edge(&self, source: &str, target: &str, kind: DependencyKind) -> bool {
        self.edges.iter().any(|edge| {
            edge.sources.iter().any(|value| value == source)
                && edge.targets.iter().any(|value| value == target)
                && edge.kinds.contains(&kind)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_transferable_policy_without_project_conventions() {
        let policy: Policy = toml::from_str(
            r#"
[dependency]
require_reviewed_edges = true
ignored_packages = ["policy-checker"]

[[dependency.layers]]
name = "foundation"
directory = "foundation"
may_depend_on = ["foundation"]

[[dependency.edges]]
sources = ["scheduler"]
targets = ["clock"]
kinds = ["normal", "development"]
"#,
        )
        .unwrap();
        assert!(policy.dependency.require_reviewed_edges);
        assert_eq!(policy.dependency.layer("foundation").unwrap().name, "foundation");
        assert!(
            policy
                .dependency
                .permits_edge("scheduler", "clock", DependencyKind::Development)
        );
        assert!(
            !policy
                .dependency
                .permits_edge("clock", "scheduler", DependencyKind::Normal)
        );
    }

    #[test]
    fn rejects_misspelled_policy_fields() {
        let error = toml::from_str::<Policy>("require_review_edges = true").unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }
}
