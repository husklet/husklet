use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
};

use crate::{
    LintError, Result,
    model::{Finding, Related, Review, Severity},
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
pub struct Direction;
impl Rule for Direction {
    fn id(&self) -> &'static str {
        "dependency-direction"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let graph = Graph::load(workspace.paths())?;
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
    layer: Layer,
    dependencies: Vec<Dependency>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Layer {
    Application,
    Container,
    Package,
    Runtime,
    Workspace,
    Other,
}

impl Layer {
    fn from_manifest(path: &Path) -> Self {
        let components = path
            .components()
            .filter_map(|component| component.as_os_str().to_str())
            .collect::<Vec<_>>();
        let Some(index) = components.iter().rposition(|component| *component == "src") else {
            return Self::Other;
        };
        match components.get(index + 1).copied() {
            Some("app" | "apps") => Self::Application,
            Some("containers") => Self::Container,
            Some("packages") => Self::Package,
            Some("runtime") => Self::Runtime,
            Some("workspaces") => Self::Workspace,
            None => Self::Other,
            Some(_) => Self::Other,
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Application => "application",
            Self::Container => "containers",
            Self::Package => "packages",
            Self::Runtime => "runtime",
            Self::Workspace => "workspaces",
            Self::Other => "other",
        }
    }
}

struct Graph {
    packages: BTreeMap<String, Package>,
    manifests: HashMap<PathBuf, String>,
}

impl Graph {
    fn load(paths: &[PathBuf]) -> Result<Self> {
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
            if name == "hl-design-lint" {
                continue;
            }
            let dependencies = dependencies(&document, &manifest, &workspace_dependencies);
            packages.insert(
                name.to_owned(),
                Package {
                    name: name.to_owned(),
                    layer: Layer::from_manifest(&manifest),
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
        Ok(Self { packages, manifests })
    }

    fn direction_findings(&self, rule: &'static str) -> Vec<Finding> {
        let mut findings = Vec::new();
        for package in self.packages.values() {
            for dependency in &package.dependencies {
                let Some(target) = self.target(dependency) else {
                    continue;
                };
                let layer_violation = match (&package.layer, &target.layer) {
                    (Layer::Package, Layer::Runtime | Layer::Container | Layer::Workspace | Layer::Application) => {
                        Some(
                            "transferable packages must not depend on runtime, container, workspace, or application code",
                        )
                    }
                    (Layer::Runtime, Layer::Container | Layer::Workspace | Layer::Application) => {
                        Some("runtime packages must not depend on container, workspace, or application code")
                    }
                    (Layer::Container, Layer::Workspace | Layer::Application) => {
                        Some("container packages must not depend on workspace or application code")
                    }
                    (Layer::Workspace, Layer::Container | Layer::Application) => {
                        Some("workspace packages must not depend on container or application code")
                    }
                    (_, Layer::Application) => Some("the application composition root must never be a dependency"),
                    _ => None,
                };
                let reviewed = allowed_edge(&package.name, &target.name)
                    || (dependency.kind == Kind::Development && allowed_development_edge(&package.name, &target.name));
                let policy_violation =
                    (!reviewed).then_some("the local dependency is not present in the checked engine package graph");
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
                finding.help = match (layer_violation, &package.layer) {
                    (Some(_), Layer::Package) => "move engine policy into `src/runtime`, retain only the transferable mechanism, or invert the edge through a runtime-owned port".into(),
                    (Some(_), _) => "keep concrete composition in `src/app/hl-engine`; expose the required capability from its owning runtime package".into(),
                    (None, _) => "remove the edge, invert it through a consumer-owned port, or update the reviewed package graph before adding the dependency".into(),
                };
                finding.related.push(Related {
                    label: format!("dependency target in {} layer", target.layer.label()),
                    location: location::package(target),
                });
                let mut review = Review::error();
                review.metadata.extend([
                    ("Source layer".into(), package.layer.label().into()),
                    ("Target layer".into(), target.layer.label().into()),
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
}

/// The reviewed production graph from `AGENTS.md` and the integrated workspace.
///
/// This is deliberately an edge list rather than a layer ordering. Runtime
/// packages are peers by placement, but their domain contracts still have one
/// exact dependency direction. Normal, build, development, target-specific,
/// renamed, and workspace-inherited local dependencies all resolve to this
/// same source/target pair before reaching this check.
fn allowed_edge(source: &str, target: &str) -> bool {
    matches!(
        (source, target),
        // Container services and Docker-compatible APIs.
        ("hl-client", "hl-container")
            | ("hl-client", "hl-daemon")
            | ("hl-client", "hl-images")
            | ("hl-client", "hl-log")
            | ("hl-container", "hl-engine")
            | ("hl-engine", "hl-native")
            | ("hl-container", "hl-fs")
            | ("hl-container", "hl-images")
            | ("hl-container", "hl-log")
            | ("hl-daemon", "hl-client")
            | ("hl-daemon", "hl-container")
            | ("hl-daemon", "hl-design")
            | ("hl-daemon", "hl-images")
            | ("hl-daemon", "hl-log")
            | ("hl-images", "hl-fs")
            | ("hl-images", "hl-log")
            // Workspace and terminal capabilities.
            | ("hl-ws-term", "hl-fs")
            | ("hl-ws-term", "hl-ws")
            // Product composition root.
            | ("husklet", "hl-client")
            | ("husklet", "hl-container")
            | ("husklet", "hl-daemon")
            | ("husklet", "hl-design")
            | ("husklet", "hl-fs")
            | ("husklet", "hl-gui")
            | ("husklet", "hl-images")
            | ("husklet", "hl-log")
            | ("husklet", "hl-ws")
            | ("husklet", "hl-ws-term")
            | ("dockerd", "hl-container")
            | ("dockerd", "hl-daemon")
            | ("dockerd", "hl-images")
            | ("dockerd", "hl-log")
            | ("engine", "hl-engine")
            | ("engine", "hl-log")
            | ("testing", "hl-checkpoint")
            | ("testing", "hl-container")
            | ("testing", "hl-descriptor")
            | ("testing", "hl-design")
            | ("testing", "hl-engine")
            | ("testing", "hl-images")
            | ("testing", "hl-log")
            | ("testing", "hl-network")
            | ("testing", "hl-process")
            | ("testing", "hl-provider")
            | ("testing", "hl-runtime")
        // Runtime foundations and domains.
            | ("hl-vfs", "hl-descriptor")
            | ("hl-vfs", "hl-fs")
            | ("hl-terminal", "hl-descriptor")
            | ("hl-event", "hl-descriptor")
            | ("hl-event", "hl-time")
            | ("hl-memory", "hl-isa")
            | ("hl-sync", "hl-memory")
            | ("hl-sync", "hl-time")
            | ("hl-network", "hl-descriptor")
            | ("hl-network", "hl-sync")
            | ("hl-ipc", "hl-descriptor")
            | ("hl-ipc", "hl-memory")
            | ("hl-ipc", "hl-sync")
            | ("hl-ipc", "hl-time")
            | ("hl-task", "hl-descriptor")
            | ("hl-task", "hl-memory")
            | ("hl-task", "hl-sync")
            | ("hl-task", "hl-time")
            | ("hl-loader", "hl-isa")
            | ("hl-loader", "hl-vfs")
            | ("hl-loader", "hl-memory")
            | ("hl-execution", "hl-isa")
            | ("hl-execution", "hl-memory")
            | ("hl-execution", "hl-softfloat")
            | ("hl-provider", "hl-descriptor")
            // Linux personality.
            | ("hl-linux", "hl-isa")
            | ("hl-linux", "hl-time")
            | ("hl-linux", "hl-descriptor")
            | ("hl-linux", "hl-vfs")
            | ("hl-linux", "hl-event")
            | ("hl-linux", "hl-memory")
            | ("hl-linux", "hl-sync")
            | ("hl-linux", "hl-network")
            | ("hl-linux", "hl-ipc")
            | ("hl-linux", "hl-task")
            // Aggregate checkpoint coordination.
            | ("hl-checkpoint", "hl-descriptor")
            | ("hl-checkpoint", "hl-vfs")
            | ("hl-checkpoint", "hl-event")
            | ("hl-checkpoint", "hl-memory")
            | ("hl-checkpoint", "hl-sync")
            | ("hl-checkpoint", "hl-network")
            | ("hl-checkpoint", "hl-ipc")
            | ("hl-checkpoint", "hl-task")
            | ("hl-checkpoint", "hl-provider")
            | ("hl-checkpoint", "hl-execution")
            // Runtime and product composition.
            | ("hl-runtime", "hl-aio")
            | ("hl-runtime", "hl-linux")
            | ("hl-runtime", "hl-loader")
            | ("hl-runtime", "hl-checkpoint")
            | ("hl-runtime", "hl-provider")
            | ("hl-runtime", "hl-execution")
            | ("hl-runtime", "hl-descriptor")
            | ("hl-runtime", "hl-event")
            | ("hl-runtime", "hl-isa")
            | ("hl-runtime", "hl-time")
            | ("hl-runtime", "hl-sync")
            | ("hl-runtime", "hl-task")
            | ("hl-runtime", "hl-memory")
            | ("hl-runtime", "hl-network")
            | ("hl-runtime", "hl-ipc")
            | ("hl-runtime", "hl-vfs")
            | ("hl-runtime", "hl-terminal")
            | ("hl-runtime", "hl-log")
            | ("hl-fake-host", "hl-descriptor")
            | ("hl-fake-host", "hl-execution")
            | ("hl-fake-host", "hl-isa")
            | ("hl-fake-host", "hl-linux")
            | ("hl-fake-host", "hl-memory")
            | ("hl-fake-host", "hl-network")
            | ("hl-fake-host", "hl-provider")
            | ("hl-fake-host", "hl-time")
            | ("hl-fake-host", "hl-vfs")
            | ("hl-engine", "hl-runtime")
            | ("hl-engine", "hl-checkpoint")
            | ("hl-engine", "hl-descriptor")
            | ("hl-engine", "hl-event")
            | ("hl-engine", "hl-network")
            | ("hl-engine", "hl-linux")
            | ("hl-engine", "hl-execution")
            | ("hl-engine", "hl-fs")
            | ("hl-engine", "hl-isa")
            | ("hl-engine", "hl-loader")
            | ("hl-engine", "hl-memory")
            | ("hl-engine", "hl-sync")
            | ("hl-engine", "hl-task")
            | ("hl-engine", "hl-time")
            | ("hl-engine", "hl-log")
            | ("hl-engine", "hl-provider")
            | ("hl-engine", "hl-ipc")
    )
}

/// Test-only edges reviewed independently from the production package graph.
///
/// Cargo excludes these edges from production builds and permits development
/// dependency cycles, but they still require an explicit ownership review.
fn allowed_development_edge(source: &str, target: &str) -> bool {
    matches!(
        (source, target),
        ("dockerd", "hl-client") | ("hl-engine", "hl-fake-host")
    )
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
