use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
};

use crate::{
    LintError, Result,
    model::{Finding, Related, Review, Severity},
    policy::{DependencyKind, DependencyPolicy, LayerPolicy},
    rule::Rule,
    source::Workspace,
};

mod cycle;
mod discovery;
mod location;
mod module;
#[cfg(test)]
#[path = "test.rs"]
mod tests;
/// Enforces crate and proven module dependency direction.
pub struct Direction {
    policy: DependencyPolicy,
}

impl Direction {
    /// Creates a dependency analyzer from repository-owned policy.
    #[must_use]
    pub fn new(policy: DependencyPolicy) -> Self {
        Self { policy }
    }
}
impl Rule for Direction {
    fn id(&self) -> &'static str {
        "dependency-direction"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let graph = Graph::load(workspace.paths(), &self.policy)?;
        let mut findings = graph.direction_findings(self.id());
        findings.extend(graph.cycle_findings(self.id()));
        findings.extend(module::findings(workspace, self.id()));
        findings.sort_by(|left, right| {
            (&left.location.path, left.location.line, &left.subject).cmp(&(
                &right.location.path,
                right.location.line,
                &right.subject,
            ))
        });
        Ok(findings)
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Normal,
    Development,
    Build,
}

impl From<Kind> for DependencyKind {
    fn from(value: Kind) -> Self {
        match value {
            Kind::Normal => Self::Normal,
            Kind::Development => Self::Development,
            Kind::Build => Self::Build,
        }
    }
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Development => "development",
            Self::Build => "build",
        }
    }

    fn joins_build_cycle(self) -> bool {
        self != Self::Development
    }
}

#[derive(Clone, Debug)]
struct Dependency {
    alias: String,
    manifest: Option<PathBuf>,
    kind: Kind,
    target: Option<String>,
}

#[derive(Clone, Debug)]
struct Package {
    name: String,
    manifest: PathBuf,
    text: String,
    layer: Option<String>,
    dependencies: Vec<Dependency>,
}

fn layer_directory(path: &Path) -> Option<&str> {
    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    let Some(index) = components.iter().rposition(|component| *component == "src") else {
        return None;
    };
    components.get(index + 1).copied()
}

struct Graph {
    packages: BTreeMap<String, Package>,
    manifests: HashMap<PathBuf, String>,
    policy: DependencyPolicy,
}

impl Graph {
    fn load(paths: &[PathBuf], policy: &DependencyPolicy) -> Result<Self> {
        let manifests = discovery::manifests(paths)?;
        let workspace_dependencies = workspace_dependencies(&manifests)?;
        let mut packages = BTreeMap::new();
        for manifest in manifests {
            let text = fs::read_to_string(&manifest).map_err(|error| LintError::io("read", &manifest, error))?;
            let document = toml::from_str::<toml::Value>(&text).map_err(|error| {
                LintError::io(
                    "parse Cargo manifest",
                    &manifest,
                    std::io::Error::new(std::io::ErrorKind::InvalidData, error),
                )
            })?;
            let Some(name) = document
                .get("package")
                .and_then(toml::Value::as_table)
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str)
            else {
                continue;
            };
            if policy.ignored_packages.iter().any(|ignored| ignored == name) {
                continue;
            }
            let dependencies = dependencies(&document, &manifest, &workspace_dependencies);
            packages.insert(
                name.to_owned(),
                Package {
                    name: name.to_owned(),
                    layer: layer_directory(&manifest)
                        .and_then(|directory| policy.layer(directory))
                        .map(|layer| layer.name.clone()),
                    manifest,
                    text,
                    dependencies,
                },
            );
        }
        let manifests = packages
            .values()
            .filter_map(|package| normalized(&package.manifest).map(|manifest| (manifest, package.name.clone())))
            .collect();
        Ok(Self {
            packages,
            manifests,
            policy: policy.clone(),
        })
    }

    fn direction_findings(&self, rule: &'static str) -> Vec<Finding> {
        let mut findings = Vec::new();
        for package in self.packages.values() {
            for dependency in &package.dependencies {
                let Some(target) = self.target(dependency) else {
                    continue;
                };
                let source_layer = package.layer.as_deref().and_then(|name| self.layer(name));
                let layer_violation = source_layer
                    .zip(target.layer.as_deref())
                    .filter(|(source, target)| !source.may_depend_on.iter().any(|allowed| allowed == *target))
                    .map(|_| "the repository layer policy forbids this dependency direction");
                let reviewed = self
                    .policy
                    .permits_edge(&package.name, &target.name, dependency.kind.into());
                let policy_violation = (self.policy.require_reviewed_edges && !reviewed)
                    .then_some("the local dependency is absent from the repository policy");
                let Some(message) = layer_violation.or(policy_violation) else {
                    continue;
                };
                let mut finding = Finding::error(
                    rule,
                    format!("{} -> {}", package.name, target.name),
                    location::dependency(package, dependency),
                );
                finding.message = format!(
                    "{message}; `{}` has a {} dependency on `{}`{}",
                    package.name,
                    dependency.kind.label(),
                    target.name,
                    dependency
                        .target
                        .as_deref()
                        .map(|target| format!(" under target `{target}`"))
                        .unwrap_or_default(),
                );
                finding.help = match layer_violation {
                    Some(_) => "invert the edge through a lower-layer port or update the repository-owned layer policy after review".into(),
                    None => format!(
                        "remove the edge, invert it through a consumer-owned port, or record `{} -> {}` in the repository policy",
                        package.name, target.name
                    ),
                };
                finding.related.push(Related {
                    label: format!(
                        "dependency target in {} layer",
                        target.layer.as_deref().unwrap_or("unclassified")
                    ),
                    location: location::package(target),
                });
                let mut review = Review::error();
                review.metadata.extend([
                    (
                        "Source layer".into(),
                        package.layer.clone().unwrap_or_else(|| "unclassified".into()),
                    ),
                    (
                        "Target layer".into(),
                        target.layer.clone().unwrap_or_else(|| "unclassified".into()),
                    ),
                    ("Dependency kind".into(), dependency.kind.label().into()),
                    ("Cargo alias".into(), dependency.alias.clone()),
                    (
                        "Target condition".into(),
                        dependency.target.clone().unwrap_or_else(|| "all".into()),
                    ),
                ]);
                review.dependencies.push(target.name.clone());
                review.questions.push(
                    "Which domain owns the capability, and can the dependency be inverted through a narrow port?"
                        .into(),
                );
                finding.review = Some(review);
                findings.push(finding);
            }
        }
        findings
    }

    fn cycle_findings(&self, rule: &'static str) -> Vec<Finding> {
        let mut edges: HashMap<&str, Vec<(&str, &Dependency)>> = HashMap::new();
        for package in self.packages.values() {
            for dependency in &package.dependencies {
                if dependency.kind.joins_build_cycle() {
                    let Some(target) = self.target(dependency) else {
                        continue;
                    };
                    edges.entry(&package.name).or_default().push((&target.name, dependency));
                }
            }
        }

        let components = cycle::components(self.packages.keys().map(String::as_str), &edges);
        let mut findings = Vec::new();
        for component in components {
            let self_cycle = component.len() == 1
                && edges
                    .get(component[0])
                    .is_some_and(|edges| edges.iter().any(|(target, _)| target == &component[0]));
            if component.len() < 2 && !self_cycle {
                continue;
            }
            let members = component.iter().copied().collect::<BTreeSet<_>>();
            let path = cycle::path(component[0], &members, &edges).unwrap_or_else(|| component.clone());
            let cycle = path.join(" -> ");
            let package = &self.packages[component[0]];
            let mut finding = Finding::error(rule, format!("crate cycle: {cycle}"), location::package(package));
            finding.message = format!(
                "workspace crates form a normal/build dependency cycle: {cycle}; development-only edges are excluded because Cargo permits dev-dependency cycles"
            );
            finding.help =
                "move the shared contract to its owning lower layer or invert one edge through a narrow trait".into();
            let mut review = Review::error();
            review.metadata.push(("Cycle members".into(), component.join(", ")));
            for member in component.iter().skip(1) {
                let related = &self.packages[*member];
                finding.related.push(Related {
                    label: "cycle member".into(),
                    location: location::package(related),
                });
                review.dependencies.push((*member).into());
            }
            review
                .questions
                .push("Which member owns the contract that currently points both directions?".into());
            finding.review = Some(review);
            findings.push(finding);
        }
        findings
    }

    fn target(&self, dependency: &Dependency) -> Option<&Package> {
        let manifest = dependency.manifest.as_ref().and_then(|path| normalized(path))?;
        let name = self.manifests.get(&manifest)?;
        self.packages.get(name)
    }

    fn layer(&self, name: &str) -> Option<&LayerPolicy> {
        self.policy.layers.iter().find(|layer| layer.name == name)
    }
}

fn dependencies(document: &toml::Value, manifest: &Path, workspace: &WorkspaceDependencies) -> Vec<Dependency> {
    let mut output = Vec::new();
    let Some(root) = document.as_table() else {
        return output;
    };
    tables(root, None, manifest, workspace, &mut output);
    if let Some(targets) = root.get("target").and_then(toml::Value::as_table) {
        for (target, value) in targets {
            if let Some(table) = value.as_table() {
                tables(table, Some(target.clone()), manifest, workspace, &mut output);
            }
        }
    }
    output
}

// The recursion hands each nested table its own owned target name.
#[allow(clippy::needless_pass_by_value)]
fn tables(
    table: &toml::map::Map<String, toml::Value>,
    target: Option<String>,
    manifest: &Path,
    workspace: &WorkspaceDependencies,
    output: &mut Vec<Dependency>,
) {
    for (section, kind) in [
        ("dependencies", Kind::Normal),
        ("dev-dependencies", Kind::Development),
        ("build-dependencies", Kind::Build),
    ] {
        let Some(values) = table.get(section).and_then(toml::Value::as_table) else {
            continue;
        };
        for (alias, specification) in values {
            let inherited = specification
                .as_table()
                .and_then(|value| value.get("workspace"))
                .and_then(toml::Value::as_bool)
                .filter(|enabled| *enabled)
                .and_then(|_| workspace.get(manifest, alias));
            let local_manifest = specification
                .as_table()
                .and_then(|value| value.get("path"))
                .and_then(toml::Value::as_str)
                .map(|path| manifest.parent().unwrap_or(Path::new("")).join(path).join("Cargo.toml"))
                .or_else(|| inherited.and_then(|dependency| dependency.manifest.clone()));
            output.push(Dependency {
                alias: alias.clone(),
                manifest: local_manifest,
                kind,
                target: target.clone(),
            });
        }
    }
}

#[derive(Clone, Debug)]
struct WorkspaceDependency {
    manifest: Option<PathBuf>,
}

struct WorkspaceDependencies {
    roots: Vec<(PathBuf, HashMap<String, WorkspaceDependency>)>,
}

impl WorkspaceDependencies {
    fn get(&self, manifest: &Path, alias: &str) -> Option<&WorkspaceDependency> {
        self.roots
            .iter()
            .filter(|(root, _)| manifest.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .and_then(|(_, dependencies)| dependencies.get(alias))
    }
}

fn workspace_dependencies(manifests: &[PathBuf]) -> Result<WorkspaceDependencies> {
    let mut roots = Vec::new();
    for manifest in manifests {
        let text = fs::read_to_string(manifest).map_err(|error| LintError::io("read", manifest, error))?;
        let document = toml::from_str::<toml::Value>(&text).map_err(|error| {
            LintError::io(
                "parse Cargo manifest",
                manifest,
                std::io::Error::new(std::io::ErrorKind::InvalidData, error),
            )
        })?;
        let Some(values) = document
            .get("workspace")
            .and_then(|workspace| workspace.get("dependencies"))
            .and_then(toml::Value::as_table)
        else {
            continue;
        };
        let mut dependencies = HashMap::new();
        for (alias, specification) in values {
            let local_manifest = specification
                .as_table()
                .and_then(|value| value.get("path"))
                .and_then(toml::Value::as_str)
                .map(|path| manifest.parent().unwrap_or(Path::new("")).join(path).join("Cargo.toml"));
            dependencies.insert(
                alias.clone(),
                WorkspaceDependency {
                    manifest: local_manifest,
                },
            );
        }
        roots.push((manifest.parent().unwrap_or(Path::new("")).to_owned(), dependencies));
    }
    Ok(WorkspaceDependencies { roots })
}

fn normalized(path: &Path) -> Option<PathBuf> {
    fs::canonicalize(path).ok()
}
