//! Extensible repository design linting with registered rules and interchangeable reporters.

#![warn(missing_docs)]
// Rule findings assign their help text once, on a cold path; `clone_into` would obscure
// the literal each rule is documenting.
#![allow(clippy::assigning_clones)]

mod error;
mod model;
mod policy;
mod report;
mod rule;
mod source;

use std::path::PathBuf;

pub use error::{LintError, Result};
pub use model::{Finding, Location, Related, Review, ReviewState, Severity, Summary};
pub use policy::{
    BoundaryPolicy, DependencyPolicy, DocumentationPolicy, LayerPolicy, OwnershipPolicy, PackageDependencyBudget,
    Policy, SourcePolicy, SourceSelector,
};
pub use report::{Cases, Diagnostic, Markdown, Reporter};
pub use rule::{
    AccessorBloat, AsyncBlocking, BooleanState, BroadTrait, CAnalyzerConfig, CCallPolicy, CPolicy, CStructure,
    CatchAllModule, CatchAllSourcePath, CeremonialStructure, ConstructorOwnership, DependencyDirection, Documentation,
    DuplicateEntity, EmptyDirectory, EnvironmentAccess, FileLength, FileName, FiniteStateString, FolderNoun,
    FreeFunction, GodObject, GuiToolkitLeakage, IgnoredResult, IntegrationCandidate, ManualDispatch, MaximumNesting,
    ModelDuplication, ModulePrefix, PathModules, PlatformCommand, PrefixDirectory, ReceiverRepetition, Registry,
    RepositoryEscape, Rule, RuntimeTool, SingleFileDirectory, StructNaming, SuffixRole, TestDependency, TestDirectory,
    TestName, UnsafeBoundary, run_c_analyzers,
};
pub use source::{Source, Workspace};

/// Runs a registry of design rules over one parsed workspace.
pub struct Linter {
    registry: Registry,
    source_policy: policy::SourcePolicy,
}

impl Linter {
    /// Creates a linter from an explicit rule registry.
    #[must_use]
    pub fn new(registry: Registry) -> Self {
        Self {
            registry,
            source_policy: policy::SourcePolicy::default(),
        }
    }

    /// Creates the repository's standard rule set.
    #[must_use]
    pub fn standard() -> Self {
        Self::standard_with_policy(Policy::default())
    }

    /// Creates the standard generic rules with an explicit repository dependency policy.
    #[must_use]
    pub fn standard_with_policy(policy: Policy) -> Self {
        let Policy {
            documentation,
            dependency,
            unsafe_boundary,
            environment_boundary,
            command_boundary,
            ownership,
            source,
        } = policy;
        let escape = rule::RepositoryEscape::new(&source);
        let mut linter = Self::new(
            Registry::new()
                .register(rule::DependencyDirection::new(dependency))
                .register(rule::Documentation::new(
                    documentation,
                    source.ignored_directories.clone(),
                ))
                .register(rule::RuntimeTool::new(ownership))
                .register(rule::UnsafeBoundary::new(unsafe_boundary))
                .register(rule::FreeFunction)
                .register(rule::ConstructorOwnership)
                .register(rule::DuplicateEntity)
                .register(rule::BooleanState)
                .register(rule::BroadTrait)
                .register(rule::EnvironmentAccess::new(environment_boundary))
                .register(escape)
                .register(rule::ManualDispatch)
                .register(rule::PlatformCommand::new(command_boundary))
                .register(rule::IgnoredResult)
                .register(rule::AsyncBlocking)
                .register(rule::StructNaming)
                .register(rule::ReceiverRepetition)
                .register(rule::GuiToolkitLeakage)
                .register(rule::GodObject)
                .register(rule::AccessorBloat)
                .register(rule::ModelDuplication)
                .register(rule::MaximumNesting)
                .register(rule::FileLength)
                .register(rule::FileName)
                .register(rule::ParentName)
                .register(rule::TestName)
                .register(rule::PrefixDirectory)
                .register(rule::SuffixRole)
                .register(rule::PathModules)
                .register(rule::TestDirectory)
                .register(rule::TestDependency)
                .register(rule::TestSuiteKebabPath)
                .register(rule::IntegrationCandidate)
                .register(rule::FolderNoun)
                .register(rule::ModulePrefix)
                .register(rule::FiniteStateString)
                .register(rule::CatchAllModule)
                .register(rule::CatchAllSourcePath)
                .register(rule::EmptyDirectory)
                .register(rule::SingleFileDirectory)
                .register(rule::CeremonialStructure)
                .register(rule::CStructure)
                .register(rule::CPolicy::new()),
        );
        linter.source_policy = source;
        linter
    }

    /// Runs every registered rule and reports its findings.
    pub fn run(&self, paths: impl IntoIterator<Item = PathBuf>, reporter: &mut dyn Reporter) -> Result<Vec<Summary>> {
        let workspace = Workspace::load_with_policy(paths, &self.source_policy)?;
        reporter.begin(&workspace)?;
        let mut summaries = Vec::new();
        for rule in self.registry.rules() {
            let findings = rule.check(&workspace)?;
            let severity = rule.severity();
            for finding in &findings {
                reporter.finding(finding)?;
            }
            summaries.push(Summary {
                rule: rule.id(),
                severity,
                findings: findings.iter().filter(|finding| finding.is_violation()).count(),
            });
        }
        reporter.finish(&summaries)?;
        Ok(summaries)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    struct Memory(Vec<Finding>);

    impl Reporter for Memory {
        fn finding(&mut self, finding: &Finding) -> Result<()> {
            self.0.push(finding.clone());
            Ok(())
        }

        fn finish(&mut self, _summaries: &[Summary]) -> Result<()> {
            Ok(())
        }
    }

    fn temporary(name: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "hl-design-lint-{name}-{}-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"lint-fixture\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        root
    }

    fn write(root: &Path, source: &str) -> PathBuf {
        let path = root.join("src/lib.rs");
        fs::write(&path, source).unwrap();
        path
    }

    fn findings(source: &str, rule: &str) -> Vec<Finding> {
        let root = temporary("rule");
        let source = write(&root, source);
        let mut reporter = Memory(Vec::new());
        Linter::standard().run([source], &mut reporter).unwrap();
        fs::remove_dir_all(root).unwrap();
        reporter.0.into_iter().filter(|finding| finding.rule == rule).collect()
    }

    #[test]
    fn registry_reporter_contract() {
        let root = temporary("registry");
        let source = write(
            &root,
            "fn sample() { let _ = std::env::var(\"X\"); if true { if true { if true {} } } }",
        );
        let mut reporter = Memory(Vec::new());
        let summaries = Linter::standard().run([source], &mut reporter).unwrap();
        let expected_ids = [
            "dependency-direction",
            "documentation-contract",
            "runtime-tool-ownership",
            "unsafe-boundary",
            "unclassified-free-function",
            "detached-constructor",
            "duplicate-entity-base",
            "boolean-state-cluster",
            "broad-trait-responsibilities",
            "environment-variable-access",
            "repository-escape",
            "manual-cli-dispatch",
            "platform-command-boundary",
            "ignored-fallible-result",
            "async-blocking-operation",
            "struct-noun-naming",
            "receiver-name-repetition",
            "gui-toolkit-type-leakage",
            "god-object-growth",
            "redundant-accessor",
            "wire-domain-model-duplication",
            "maximum-nesting",
            "file-length",
            "file-name-density",
            "redundant-parent-name",
            "singular-test-file",
            "flat-prefix-density",
            "flat-role-density",
            "path-module-flattening",
            "test-only-source-directory",
            "sibling-test-dependency",
            "test-suite-kebab-path",
            "integration-test-candidate",
            "folder-noun",
            "redundant-module-prefix",
            "string-backed-finite-state",
            "catch-all-module-name",
            "catch-all-source-path",
            "empty-directory",
            "single-file-directory",
            "ceremonial-structure",
            "c-source-structure",
            "c-source-policy",
        ];
        assert_eq!(
            summaries.iter().map(|summary| summary.rule).collect::<Vec<_>>(),
            expected_ids
        );
        assert_eq!(reporter.0.len(), 2);
        assert_eq!(reporter.0[0].rule, "environment-variable-access");
        assert_eq!(reporter.0[1].rule, "maximum-nesting");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn standard_registry_executes_embedded_c_rules() {
        let root = temporary("standard-c");
        let path = root.join("src/oversized.c");
        let mut source = String::from("int oversized(void) {\n");
        for _ in 0..=200 {
            source.push_str("  value += 1;\n");
        }
        source.push_str("  return value;\n}\n");
        fs::write(&path, source).unwrap();
        let mut reporter = Memory(Vec::new());
        Linter::standard().run([path], &mut reporter).unwrap();
        assert!(
            reporter.0.iter().any(|finding| finding.rule == "c-function-length"),
            "the normal registry must execute C analysis"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn standard_rule_semantics() {
        let root = temporary("rules");
        let source = write(
            &root,
            r#"
struct Changed;
struct Image { id: u64, name: String, path: String }
struct DiscoveredImage { id: u64, name: String, path: String, score: u8 }

struct Subject;
fn unclassified(value: &Subject) -> usize { let _ = value; 1 }
struct Constructed;
fn construct() -> Constructed { Constructed }
#[hl_design::classify(pkg)]
fn classified(value: &Subject) -> usize { let _ = value; 1 }
fn once(value: usize) -> usize { value + 1 }

fn caller() {
    let _ = unclassified(&Subject);
    let _ = classified(&Subject);
    let _ = once(1);
    let _ = std::env::var_os("HL_TEST");
    if true { if true { if true {} } }
}
"#,
        );
        let mut reporter = Memory(Vec::new());
        let summaries = Linter::standard().run([source], &mut reporter).unwrap();

        assert_eq!(summaries.len(), 43);
        assert!(
            reporter
                .0
                .iter()
                .any(|finding| finding.rule == "unclassified-free-function"
                    && finding.subject == "unclassified"
                    && finding.is_violation())
        );
        assert!(
            reporter
                .0
                .iter()
                .any(|finding| finding.rule == "detached-constructor" && finding.subject == "construct")
        );
        assert!(
            reporter
                .0
                .iter()
                .any(|finding| finding.rule == "unclassified-free-function"
                    && finding.subject == "classified"
                    && !finding.is_violation())
        );
        for rule in [
            "duplicate-entity-base",
            "struct-noun-naming",
            "environment-variable-access",
            "maximum-nesting",
        ] {
            assert!(reporter.0.iter().any(|finding| finding.rule == rule), "missing {rule}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cases_classified_functions() {
        let root = temporary("cases");
        let source = write(
            &root,
            r#"
struct Subject;
fn missing(value: &Subject) -> usize { let _ = value; 1 }
#[hl_design::classify(domain = "surface")]
fn reviewed(value: &Subject) -> usize { let _ = value; 1 }
fn caller() { let _ = missing(&Subject); let _ = reviewed(&Subject); }
"#,
        );
        let queues = root.join("lint");
        fs::create_dir_all(queues.join("errors")).unwrap();
        fs::create_dir_all(queues.join("check")).unwrap();
        fs::write(queues.join("errors/.gitkeep"), "").unwrap();
        fs::write(queues.join("check/.gitkeep"), "").unwrap();
        fs::write(queues.join("errors/stale.md"), "stale").unwrap();
        fs::write(queues.join("check/stale.md"), "stale").unwrap();

        let mut reporter = Cases::with_output(queues.clone(), Vec::new());
        Linter::standard().run([source], &mut reporter).unwrap();
        let errors = fs::read_dir(queues.join("errors"))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        let checks = fs::read_dir(queues.join("check"))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(errors.len(), 2);
        assert_eq!(checks.len(), 2);
        assert!(queues.join("errors/.gitkeep").is_file());
        assert!(queues.join("check/.gitkeep").is_file());
        assert!(!queues.join("errors/stale.md").exists());
        assert!(!queues.join("check/stale.md").exists());
        let check = checks
            .iter()
            .find(|entry| entry.file_name() != ".gitkeep")
            .map(|entry| fs::read_to_string(entry.path()).unwrap())
            .unwrap();
        assert!(check.contains("domain(surface)"));
        assert!(check.contains("## Related context"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn free_classification_contracts() {
        let values = findings(
            r#"
struct AppState;
type Payload = usize;
fn zero() {}
fn one(value: Payload) { let _ = value; }
fn two(left: Payload, right: usize) { let _ = (left, right); }
fn three(a: Payload, b: usize, c: usize) { let _ = (a, b, c); }
extern "C" fn ffi(value: Payload) { let _ = value; }
#[hl_design::adapter] async fn handler(State(state): State<AppState>) { let _ = state; }
async fn unreviewed_handler(State(state): State<AppState>) { let _ = state; }
fn detached(state: AppState) { let _ = state; }
#[hl_design::adapter] fn parses(value: &str) -> Result<usize, String> { Ok(value.len()) }
fn unmarked_parses(value: &str) -> Result<usize, String> { Ok(value.len()) }
#[cfg(unix)] fn gated(value: Payload) { let _ = value; }
#[cfg(windows)] fn gated(value: Payload) { let _ = value; }
#[cfg(test)] fn test_only(value: Payload) { let _ = value; }
#[hl_design::classify(pkg)] fn package(value: Payload) { let _ = value; }
#[hl_design::classify(domain = "gpu")] fn domain(value: Payload) { let _ = value; }
#[hl_design::classify(domain = "")] fn malformed(value: Payload) { let _ = value; }
"#,
            "unclassified-free-function",
        );
        assert_eq!(
            values
                .iter()
                .map(|finding| finding.subject.as_str())
                .collect::<Vec<_>>(),
            ["one", "detached", "gated", "package", "domain", "malformed"]
        );
        assert!(
            values
                .iter()
                .find(|value| value.subject == "one")
                .unwrap()
                .is_violation()
        );
        let gated = values.iter().find(|value| value.subject == "gated").unwrap();
        assert!(
            gated
                .review
                .as_ref()
                .unwrap()
                .metadata
                .iter()
                .any(|(key, value)| key == "Usage resolution" && value == "unique name in scanned tree")
        );
        let package = values.iter().find(|value| value.subject == "package").unwrap();
        assert!(!package.is_violation());
        assert!(matches!(
            package.review.as_ref().map(|review| &review.state),
            Some(ReviewState::Check(value)) if value == "pkg(lint-fixture)"
        ));
        assert!(
            !values
                .iter()
                .find(|value| value.subject == "domain")
                .unwrap()
                .is_violation()
        );
        assert!(
            values
                .iter()
                .find(|value| value.subject == "malformed")
                .unwrap()
                .is_violation()
        );
    }

    #[test]
    fn environment_builtin_macros() {
        let values = findings(
            r#"
fn reads() {
    let _ = std::env::var("A");
    let _ = std::env::var_os("B");
    let _ = std::env::vars();
    let _ = std::env::vars_os();
    // Substituted by the compiler, so not ambient process input.
    let _ = env!("C");
    let _ = option_env!("D");
}
"#,
            "environment-variable-access",
        );
        assert_eq!(values.len(), 4);
        assert!(values.iter().all(Finding::is_violation));
        assert!(values.iter().all(|finding| !finding.related.is_empty()));
    }

    #[test]
    fn nesting_two_levels() {
        let values = findings(
            r"
fn shallow(value: bool) {
    if value {} else if value {} else if value {}
}
fn deep(value: bool) {
    if value { for _ in 0..1 { match value { true => {}, false => {} } } }
}
",
            "maximum-nesting",
        );
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].subject, "deep");
    }

    #[test]
    fn duplicate_typed_fields() {
        let values = findings(
            r"
struct Image { id: u64, name: String, path: String }
struct DiscoveredImage { id: u64, name: String, path: String, score: u8 }
struct Unrelated { id: u64, name: String, path: String }
struct WrongTypes { id: String, name: String, path: String }
",
            "duplicate-entity-base",
        );
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].subject, "Image_DiscoveredImage");
        assert_eq!(values[0].related.len(), 1);
    }

    #[test]
    fn naming_types_methods() {
        let values = findings(
            r#"
struct Workspace;
struct Selected;
#[hl_design::naming(reason = "external protocol vocabulary")]
struct Updated;
struct VkImageCopy2;
enum Changed { Value }
type Chosen = usize;
struct Workspaces;
impl Workspaces {
    fn workspace(&self, id: usize) { let _ = id; }
    fn remove(&self, id: usize) { let _ = id; }
    fn from_items(_: Vec<Workspace>) -> Self { Self }
}
"#,
            "struct-noun-naming",
        );
        assert_eq!(values.len(), 1);
        assert!(values.iter().any(|finding| finding.subject == "Selected"));
        assert!(!values.iter().any(|finding| finding.subject == "workspace"));
        assert!(!values.iter().any(|finding| finding.subject == "Updated"));
        assert!(!values.iter().any(|finding| finding.subject == "remove"));
        assert!(!values.iter().any(|finding| finding.subject == "from_items"));
    }

    #[test]
    fn receiver_versions_exclusions() {
        let values = findings(
            r"
struct Directory;
impl Directory {
    fn create_directory(&self) {}
    fn directory_remove(&self) {}
    fn create_file(&self) {}
    fn from_directory(_: Directory) -> Self { Self }
    fn into_directory(self) -> Directory { self }
    fn try_into_directory(self) -> Result<Directory, ()> { Ok(self) }
    fn try_again_directory(&self) {}
}

struct HTTPServerV2;
impl HTTPServerV2 {
    fn restart_http_server_v2(&self) {}
    fn restart_http_server(&self) {}
}

struct Id;
impl Id {
    fn parse_id(&self) {}
}

trait Workspace {
    fn remove_workspace(&self);
    fn workspace_settings(&self);
}

trait Foreign {
    fn remove_directory(&self);
}
impl Foreign for Directory {
    fn remove_directory(&self) {}
}
",
            "receiver-name-repetition",
        );
        let subjects = values
            .iter()
            .map(|finding| finding.subject.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            subjects,
            [
                "Directory::create_directory",
                "Directory::directory_remove",
                "Directory::try_again_directory",
                "HTTPServerV2::restart_http_server_v2",
                "Workspace::remove_workspace",
                "Workspace::workspace_settings",
            ]
        );
        assert!(values.iter().all(|finding| finding.review.is_some()));
        assert_eq!(values[3].review.as_ref().unwrap().metadata[1].1, "http, server, v, 2");
    }

    #[test]
    fn catch_module_identity() {
        let values = findings(
            r#"
mod util {}
mod r#common {}
mod utility {}
fn helper() {}
struct SharedState;
use external_crate::core;
const PROSE: &str = "mod misc {}";
"#,
            "catch-all-module-name",
        );
        assert_eq!(
            values
                .iter()
                .map(|finding| finding.subject.as_str())
                .collect::<Vec<_>>(),
            ["util", "common"]
        );
        assert!(values.iter().all(Finding::is_violation));
        assert!(values.iter().all(|finding| {
            finding.review.as_ref().is_some_and(|review| {
                review
                    .metadata
                    .iter()
                    .any(|(key, value)| key == "declaration" && value == "inline module declaration")
            })
        }));
    }

    #[test]
    fn catch_external_modules() {
        let root = temporary("catch-all-files");
        fs::create_dir_all(root.join("src/common")).unwrap();
        fs::create_dir_all(root.join("src/fixture")).unwrap();
        fs::write(root.join("src/lib.rs"), "mod common;\nmod helper;\n").unwrap();
        fs::write(root.join("src/common/mod.rs"), "pub struct Fixture;\n").unwrap();
        fs::write(root.join("src/helper.rs"), "pub struct Value;\n").unwrap();
        fs::write(
            root.join("src/path_root.rs"),
            "#[path = \"fixture/shared.rs\"] mod shared;\n",
        )
        .unwrap();
        fs::write(root.join("src/fixture/shared.rs"), "pub struct Input;\n").unwrap();

        let workspace = Workspace::load([root.join("src")]).unwrap();
        let values = CatchAllModule.check(&workspace).unwrap();
        assert_eq!(values.len(), 3);
        assert_eq!(
            values
                .iter()
                .map(|finding| finding.subject.as_str())
                .collect::<Vec<_>>(),
            ["common", "shared", "helper"]
        );
        assert!(values.iter().all(|finding| finding.location.line == 1));
        assert!(values.iter().all(|finding| {
            finding
                .review
                .as_ref()
                .unwrap()
                .metadata
                .iter()
                .any(|(key, value)| key == "scope" && value == "production")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn catch_test_support() {
        let root = temporary("catch-all-tests");
        let tests = root.join("tests");
        fs::create_dir_all(tests.join("common")).unwrap();
        fs::write(tests.join("common/mod.rs"), "pub struct Harness;\n").unwrap();
        fs::write(tests.join("workflow.rs"), "mod common;\n").unwrap();

        let workspace = Workspace::load([tests]).unwrap();
        let values = CatchAllModule.check(&workspace).unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].subject, "common");
        assert!(
            values[0]
                .review
                .as_ref()
                .unwrap()
                .metadata
                .contains(&("scope".to_owned(), "test".to_owned()))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reporters_injected_outputs() {
        let root = temporary("reporters");
        let source = write(
            &root,
            "struct Payload;\n#[hl_design::classify(pkg)] fn reviewed(value: Payload) { let _ = value; }\nfn missing(value: Payload) { reviewed(value); }",
        );
        let mut diagnostic = Diagnostic::new(Vec::new());
        Linter::standard().run([source.clone()], &mut diagnostic).unwrap();
        let diagnostic = String::from_utf8(diagnostic.into_inner()).unwrap();
        assert!(diagnostic.contains("free function `missing` takes one declared type"));
        assert!(!diagnostic.contains("unclassified free function `reviewed`"));

        let mut markdown = Markdown::new(Vec::new());
        Linter::standard().run([source], &mut markdown).unwrap();
        let markdown = String::from_utf8(markdown.into_inner()).unwrap();
        assert!(markdown.contains("## `reviewed`"));
        assert!(markdown.contains("Violation: `false`"));
        assert!(markdown.contains("## Summary"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn file_test_files() {
        let root = temporary("test-file-length");
        let tests = root.join("tests");
        fs::create_dir_all(&tests).unwrap();
        let path = tests.join("oversized.rs");
        fs::write(&path, format!("{}struct Fixture;\n", "\n".repeat(500))).unwrap();

        let workspace = Workspace::load([path]).unwrap();
        let findings = FileLength.check(&workspace).unwrap();

        assert!(findings.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn file_test_modules() {
        let root = temporary("inline-test-length");
        let path = root.join("src/lib.rs");
        let production = "\n".repeat(490);
        let tests = "\n".repeat(100);
        fs::write(
            &path,
            format!("{production}struct Runtime;\n#[cfg(test)] mod tests {{{tests}}}\n"),
        )
        .unwrap();

        let workspace = Workspace::load([path]).unwrap();
        let findings = FileLength.check(&workspace).unwrap();

        assert!(findings.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_placeholder_folders() {
        let root = temporary("empty-directory");
        fs::create_dir_all(root.join("src/model/unused")).unwrap();
        fs::create_dir_all(root.join("src/adapter/planned")).unwrap();
        fs::write(root.join("src/adapter/planned/.gitkeep"), "").unwrap();
        write(&root, "pub struct Present;\n");

        let workspace = Workspace::load([root.join("src")]).unwrap();
        let findings = EmptyDirectory.check(&workspace).unwrap();
        let paths = findings
            .iter()
            .map(|finding| finding.location.path.strip_prefix(&root).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            [Path::new("src/adapter/planned"), Path::new("src/model/unused"),]
        );
        assert!(findings.iter().all(Finding::is_violation));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_excluded_trees() {
        let root = temporary("empty-directory-exclusions");
        for path in [
            "lint/errors",
            "target/debug/empty",
            "vendor/package/empty",
            "src/native/execution/empty",
            "src/schema/cpu/empty",
            "src/packages/hl-design-lint/empty",
        ] {
            fs::create_dir_all(root.join(path)).unwrap();
        }
        write(&root, "pub struct Present;\n");

        let policy = policy::SourcePolicy {
            ignored_directories: vec![
                "lint".into(),
                "target".into(),
                "vendor".into(),
                "native".into(),
                "schema".into(),
            ],
            self_packages: vec!["hl-design-lint".into()],
            ..Default::default()
        };
        let workspace = Workspace::load_with_policy([root.clone()], &policy).unwrap();

        let findings = EmptyDirectory.check(&workspace).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].location.path, root.join("src/packages"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workspace_addressed_directly() {
        let root = temporary("self-exclusion");
        let linter = root.join("hl-design-lint");
        fs::create_dir_all(linter.join("src")).unwrap();
        fs::write(
            linter.join("Cargo.toml"),
            "[package]\nname = \"hl-design-lint\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        fs::write(
            linter.join("src/lib.rs"),
            "fn would_otherwise_be_reported(value: usize) { let _ = value; }",
        )
        .unwrap();
        let workspace = Workspace::load([linter.clone()]).unwrap();
        assert_eq!(workspace.sources().len(), 1);

        let policy = policy::SourcePolicy {
            self_packages: vec!["hl-design-lint".into()],
            ..Default::default()
        };
        let workspace = Workspace::load_with_policy([root.clone()], &policy).unwrap();
        assert!(workspace.sources().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workspace_source_layers() {
        let root = temporary("source-layers");
        for (layer, package, source) in [
            ("packages", "hl-fs", "fn filesystem(value: u8) {}\n"),
            ("runtime", "hl-task", "fn task(value: u8) {}\n"),
            ("app", "hl-engine", "fn engine(value: u8) {}\n"),
            ("native", "execution", "fn native(value: u8) {}\n"),
            ("schema", "cpu", "fn schema(value: u8) {}\n"),
        ] {
            let directory = root.join(format!("src/{layer}/{package}/src"));
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("lib.rs"), source).unwrap();
        }

        let policy = policy::SourcePolicy {
            foreign_source_directories: vec!["native".into(), "schema".into()],
            ..Default::default()
        };
        let workspace = Workspace::load_with_policy([root.join("src")], &policy).expect("load workspace");
        let paths = workspace
            .sources()
            .iter()
            .map(|source| source.path.as_path())
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            [
                root.join("src/app/hl-engine/src/lib.rs").as_path(),
                root.join("src/packages/hl-fs/src/lib.rs").as_path(),
                root.join("src/runtime/hl-task/src/lib.rs").as_path(),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }
}
