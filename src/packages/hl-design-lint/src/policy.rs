use std::{fs, path::Path};

use serde::Deserialize;

use crate::{LintError, Result};

/// Repository-owned policy consumed by the generic dependency analyzer.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyPolicy {
    /// Package names omitted from dependency analysis.
    #[serde(default)]
    pub ignored_packages: Vec<String>,
    /// Directory-to-layer classifications and permitted layer directions.
    #[serde(default)]
    pub layers: Vec<LayerPolicy>,
    /// Package-specific dependency budgets for unusually narrow components.
    #[serde(default)]
    pub package_budgets: Vec<PackageDependencyBudget>,
}

/// Complete, repository-owned configuration for the reusable analyzers.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Repository documentation inventory and executable example contracts.
    #[serde(default)]
    pub documentation: DocumentationPolicy,
    /// Dependency architecture.
    #[serde(default)]
    pub dependency: DependencyPolicy,
    /// Sources at which unsafe Rust is an intentional boundary.
    #[serde(default)]
    pub unsafe_boundary: BoundaryPolicy,
    /// Sources allowed to capture ambient environment state.
    #[serde(default)]
    pub environment_boundary: BoundaryPolicy,
    /// Sources allowed to construct host processes.
    #[serde(default)]
    pub command_boundary: BoundaryPolicy,
    /// Repository ownership conventions.
    #[serde(default)]
    pub ownership: OwnershipPolicy,
    /// Source discovery and repository-escape exclusions.
    #[serde(default)]
    pub source: SourcePolicy,
    /// Limits for C header interfaces.
    #[serde(default)]
    pub c_interface: CInterfacePolicy,
    /// C functions whose return values carry mandatory outcome or ownership information.
    #[serde(default)]
    pub c_result: CResultPolicy,
    /// C operations that require an attached safety rationale.
    #[serde(default)]
    pub c_safety: CSafetyPolicy,
    /// C allocation functions whose nullable results require checking before dereference.
    #[serde(default)]
    pub c_allocation: CAllocationPolicy,
}

/// Portable nullability policy for C allocation functions.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CAllocationPolicy {
    /// Exact allocator names that may return null.
    #[serde(default)]
    pub functions: Vec<String>,
}

/// Portable safety-rationale policy for C operations with caller-owned invariants.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CSafetyPolicy {
    /// Exact function names whose calls require an attached `SAFETY:` comment.
    #[serde(default)]
    pub operations: Vec<String>,
}

/// Portable must-use policy for C function results.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CResultPolicy {
    /// Exact function names whose direct call results must be consumed.
    #[serde(default)]
    pub must_use_functions: Vec<String>,
}

/// Portable limits for C header interfaces.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CInterfacePolicy {
    /// Maximum number of externally visible function declarations in one header.
    #[serde(default = "default_c_interface_functions")]
    pub maximum_functions: usize,
}

impl Default for CInterfacePolicy {
    fn default() -> Self {
        Self {
            maximum_functions: default_c_interface_functions(),
        }
    }
}

const fn default_c_interface_functions() -> usize {
    24
}

/// Repository-owned Markdown inventory and structural example documents.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentationPolicy {
    /// Slash-normalized Markdown paths permitted in the repository.
    #[serde(default)]
    pub allowed: Vec<String>,
    /// Allowed Markdown paths that must satisfy the generic example-document contract.
    #[serde(default)]
    pub examples: Vec<String>,
}

/// Portable source selectors. A source matches any selector, while each selector requires every
/// configured field to match.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundaryPolicy {
    /// Sources approved as boundaries.
    #[serde(default)]
    pub allow: Vec<SourceSelector>,
    /// Module names treated as a boundary inside sources selected by `module_owners`.
    #[serde(default)]
    pub module_names: Vec<String>,
    /// Sources in which `module_names` are approved.
    #[serde(default)]
    pub module_owners: Vec<SourceSelector>,
}

/// A source selector expressed only in Cargo and filesystem concepts.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSelector {
    /// Exact Cargo package name.
    pub package: Option<String>,
    /// Exact repository domain below `src/`.
    pub domain: Option<String>,
    /// Slash-normalized substring of the source path.
    pub path_contains: Option<String>,
    /// Exact leading Rust module path.
    #[serde(default)]
    pub module_prefix: Vec<String>,
    /// Exact file name.
    pub file: Option<String>,
}

/// Policy for classifying tools that do not belong to runtime domains.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipPolicy {
    /// Domains in which tool packages are forbidden.
    #[serde(default)]
    pub protected_domains: Vec<String>,
    /// Exact tool package names.
    #[serde(default)]
    pub tool_names: Vec<String>,
    /// Substrings classifying a package as a tool.
    #[serde(default)]
    pub tool_contains: Vec<String>,
    /// Suffixes classifying a package as a tool.
    #[serde(default)]
    pub tool_suffixes: Vec<String>,
    /// Destination domain named in remediation text.
    pub destination_domain: Option<String>,
}

/// Generic source traversal policy.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePolicy {
    /// Generated or externally owned directory names omitted during traversal.
    ///
    /// An ignored directory is opaque: the linter does not inspect it, and it
    /// does not count as repository-owned content that can justify its parent.
    #[serde(default = "default_ignored_directories")]
    pub ignored_directories: Vec<String>,
    /// Marker files whose ancestor subtree is externally owned or generated.
    /// Marked subtrees are opaque and do not count as content of their parent.
    #[serde(default)]
    pub ignored_markers: Vec<String>,
    /// Package directories omitted unless explicitly requested.
    #[serde(default)]
    pub self_packages: Vec<String>,
    /// Directories immediately below `src` that contain non-Rust/externally parsed sources.
    #[serde(default)]
    pub foreign_source_directories: Vec<String>,
}

impl Default for SourcePolicy {
    fn default() -> Self {
        Self {
            ignored_directories: default_ignored_directories(),
            ignored_markers: Vec::new(),
            self_packages: Vec::new(),
            foreign_source_directories: Vec::new(),
        }
    }
}

fn default_ignored_directories() -> Vec<String> {
    [".git", "target", "vendor"].into_iter().map(str::to_owned).collect()
}

impl Policy {
    /// Loads the complete policy from TOML.
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

impl SourceSelector {
    pub(crate) fn matches(&self, package: &str, domain: &str, path: &Path, modules: &[String]) -> bool {
        self.package.as_deref().is_none_or(|value| value == package)
            && self.domain.as_deref().is_none_or(|value| value == domain)
            && self
                .file
                .as_deref()
                .is_none_or(|value| path.file_name().and_then(|name| name.to_str()) == Some(value))
            && self.path_contains.as_ref().is_none_or(|value| {
                let path = slash(path);
                path.contains(value)
                    && (!value.ends_with("/src/ffi") || path.ends_with("/src/ffi.rs") || path.contains("/src/ffi/"))
            })
            && (self.module_prefix.is_empty() || modules.starts_with(&self.module_prefix))
    }
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
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

/// A package-specific cap on local Cargo dependencies.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageDependencyBudget {
    /// Exact Cargo package name to constrain.
    pub package: String,
    /// Maximum number of distinct local dependency packages across all kinds and targets.
    pub maximum: usize,
}

impl DependencyPolicy {
    pub(crate) fn layer(&self, directory: &str) -> Option<&LayerPolicy> {
        self.layers.iter().find(|layer| layer.directory == directory)
    }

    pub(crate) fn package_budget(&self, package: &str) -> Option<usize> {
        self.package_budgets
            .iter()
            .find(|budget| budget.package == package)
            .map(|budget| budget.maximum)
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
ignored_packages = ["policy-checker"]

[[dependency.layers]]
name = "foundation"
directory = "foundation"
may_depend_on = ["foundation"]

[[dependency.package_budgets]]
package = "translator"
maximum = 1

[c_interface]
maximum_functions = 12

[c_result]
must_use_functions = ["acquire"]

[c_safety]
operations = ["copy_bytes"]

[c_allocation]
functions = ["allocate"]
"#,
        )
        .unwrap();
        assert_eq!(policy.dependency.layer("foundation").unwrap().name, "foundation");
        assert_eq!(policy.dependency.package_budget("translator"), Some(1));
        assert_eq!(policy.c_interface.maximum_functions, 12);
        assert_eq!(policy.c_result.must_use_functions, ["acquire"]);
        assert_eq!(policy.c_safety.operations, ["copy_bytes"]);
        assert_eq!(policy.c_allocation.functions, ["allocate"]);
    }

    #[test]
    fn rejects_misspelled_policy_fields() {
        let error = toml::from_str::<Policy>("require_review_edges = true").unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn configurable_analyzers_contain_no_repository_business_literals() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/rule");
        for relative in [
            "rust/safety/mod.rs",
            "rust/environment/mod.rs",
            "rust/command/mod.rs",
            "repository/ownership/mod.rs",
            "repository/escape/mod.rs",
        ] {
            let text = fs::read_to_string(root.join(relative)).unwrap();
            for forbidden in ["husklet", "hl-engine", "hl-gpu", "src/apps/testing", "src/runtime"] {
                assert!(
                    !text.contains(forbidden),
                    "{relative} embeds project literal `{forbidden}`"
                );
            }
        }
    }
}
