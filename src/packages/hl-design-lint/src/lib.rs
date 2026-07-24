//! Extensible repository design linting with registered rules and interchangeable reporters.

#![warn(missing_docs)]

mod error;
mod model;
mod report;
mod rule;
mod source;

use std::path::PathBuf;

pub use error::{LintError, Result};
pub use model::{Finding, Location, Related, Review, ReviewState, Severity, Summary};
pub use report::{Cases, Diagnostic, Markdown, Reporter};
pub use rule::{
    AccessorBloat, AsyncBlocking, BooleanState, BroadTrait, CatchAllModule, DeepControlFlow,
    DependencyDirection, DuplicateEntity, EmptyDirectory, EnvironmentAccess, FileLength,
    FiniteStateString, FreeFunction, GodObject, GuiToolkitLeakage, IgnoredResult, ModelDuplication,
    PlatformCommand, ReceiverRepetition, Registry, Rule, SingleUse, StructNaming,
};
pub use source::{Source, Workspace};

/// Runs a registry of design rules over one parsed workspace.
pub struct Linter {
    registry: Registry,
}

impl Linter {
    /// Creates a linter from an explicit rule registry.
    pub fn new(registry: Registry) -> Self {
        Self { registry }
    }

    /// Creates the repository's standard rule set.
    pub fn standard() -> Self {
        Self::new(
            Registry::new()
                .register(rule::DependencyDirection)
                .register(rule::FreeFunction)
                .register(rule::DuplicateEntity)
                .register(rule::BooleanState)
                .register(rule::BroadTrait)
                .register(rule::EnvironmentAccess)
                .register(rule::PlatformCommand)
                .register(rule::IgnoredResult)
                .register(rule::AsyncBlocking)
                .register(rule::StructNaming)
                .register(rule::ReceiverRepetition)
                .register(rule::GuiToolkitLeakage)
                .register(rule::GodObject)
                .register(rule::AccessorBloat)
                .register(rule::ModelDuplication)
                .register(rule::SingleUse)
                .register(rule::DeepControlFlow)
                .register(rule::FileLength)
                .register(rule::FiniteStateString)
                .register(rule::CatchAllModule)
                .register(rule::EmptyDirectory),
        )
    }

    /// Runs every registered rule and reports its findings.
    pub fn run(
        &self,
        paths: impl IntoIterator<Item = PathBuf>,
        reporter: &mut dyn Reporter,
    ) -> Result<Vec<Summary>> {
        let workspace = Workspace::load(paths)?;
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
                findings: findings
                    .iter()
                    .filter(|finding| finding.is_violation())
                    .count(),
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
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
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
        reporter
            .0
            .into_iter()
            .filter(|finding| finding.rule == rule)
            .collect()
    }

    #[test]
    fn registry_runs_rules_through_one_reporter_contract() {
        let root = temporary("registry");
        let source = write(
            &root,
            "fn sample() { let _ = std::env::var(\"X\"); if true { if true { if true {} } } }",
        );
        let mut reporter = Memory(Vec::new());
        let summaries = Linter::standard().run([source], &mut reporter).unwrap();
        assert_eq!(summaries.len(), 21);
        assert_eq!(reporter.0.len(), 2);
        assert_eq!(reporter.0[0].rule, "environment-variable-access");
        assert_eq!(reporter.0[1].rule, "deep-control-flow");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn standard_registry_preserves_all_rule_semantics() {
        let root = temporary("rules");
        let source = write(
            &root,
            r#"
struct Changed;
struct Image { id: u64, name: String, path: String }
struct DiscoveredImage { id: u64, name: String, path: String, score: u8 }

fn unclassified(value: &str) -> usize { value.len() }
#[hl_design::classify(pkg)]
fn classified(value: &str) -> usize { value.len() }
fn once(value: usize) -> usize { value + 1 }

fn caller() {
    let _ = unclassified("x");
    let _ = classified("x");
    let _ = once(1);
    let _ = std::env::var_os("HL_TEST");
    if true { if true { if true {} } }
}
"#,
        );
        let mut reporter = Memory(Vec::new());
        let summaries = Linter::standard().run([source], &mut reporter).unwrap();

        assert_eq!(summaries.len(), 21);
        assert!(reporter
            .0
            .iter()
            .any(|finding| finding.rule == "unclassified-free-function"
                && finding.subject == "unclassified"
                && finding.is_violation()));
        assert!(reporter
            .0
            .iter()
            .any(|finding| finding.rule == "unclassified-free-function"
                && finding.subject == "classified"
                && !finding.is_violation()));
        for rule in [
            "duplicate-entity-base",
            "struct-noun-naming",
            "environment-variable-access",
            "single-use-free-function",
            "deep-control-flow",
        ] {
            assert!(
                reporter.0.iter().any(|finding| finding.rule == rule),
                "missing {rule}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cases_replace_both_queues_and_separate_classified_functions() {
        let root = temporary("cases");
        let source = write(
            &root,
            r#"
fn missing(value: &str) -> usize { value.len() }
#[hl_design::classify(domain = "surface")]
fn reviewed(value: &str) -> usize { value.len() }
fn caller() { let _ = missing("x"); let _ = reviewed("x"); }
"#,
        );
        let queues = root.join("lint");
        fs::create_dir_all(queues.join("errors")).unwrap();
        fs::create_dir_all(queues.join("check")).unwrap();
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
        assert_eq!(errors.len(), 1);
        assert_eq!(checks.len(), 1);
        assert!(!queues.join("errors/stale.md").exists());
        assert!(!queues.join("check/stale.md").exists());
        let check = fs::read_to_string(checks[0].path()).unwrap();
        assert!(check.contains("domain(surface)"));
        assert!(check.contains("## Related context"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn free_function_rule_honors_scope_and_classification_contracts() {
        let values = findings(
            r#"
fn zero() {}
fn one(value: usize) { let _ = value; }
fn two(left: usize, right: usize) { let _ = (left, right); }
fn three(a: usize, b: usize, c: usize) { let _ = (a, b, c); }
extern "C" fn ffi(value: usize) { let _ = value; }
#[hl_design::adapter] async fn handler(State(state): State<AppState>) { let _ = state; }
async fn unreviewed_handler(State(state): State<AppState>) { let _ = state; }
fn detached(state: AppState) { let _ = state; }
#[cfg(test)] fn test_only(value: usize) { let _ = value; }
#[hl_design::classify(pkg)] fn package(value: usize) { let _ = value; }
#[hl_design::classify(domain = "gpu")] fn domain(value: usize) { let _ = value; }
#[hl_design::classify(domain = "")] fn malformed(value: usize) { let _ = value; }
"#,
            "unclassified-free-function",
        );
        assert_eq!(
            values
                .iter()
                .map(|finding| finding.subject.as_str())
                .collect::<Vec<_>>(),
            [
                "one",
                "two",
                "unreviewed_handler",
                "detached",
                "package",
                "domain",
                "malformed"
            ]
        );
        assert!(values
            .iter()
            .find(|value| value.subject == "one")
            .unwrap()
            .is_violation());
        let package = values
            .iter()
            .find(|value| value.subject == "package")
            .unwrap();
        assert!(!package.is_violation());
        assert!(matches!(
            package.review.as_ref().map(|review| &review.state),
            Some(ReviewState::Check(value)) if value == "pkg(lint-fixture)"
        ));
        assert!(!values
            .iter()
            .find(|value| value.subject == "domain")
            .unwrap()
            .is_violation());
        assert!(values
            .iter()
            .find(|value| value.subject == "malformed")
            .unwrap()
            .is_violation());
    }

    #[test]
    fn environment_rule_covers_std_calls_and_builtin_macros() {
        let values = findings(
            r#"
fn reads() {
    let _ = std::env::var("A");
    let _ = std::env::var_os("B");
    let _ = std::env::vars();
    let _ = std::env::vars_os();
    let _ = env!("C");
    let _ = option_env!("D");
}
"#,
            "environment-variable-access",
        );
        assert_eq!(values.len(), 6);
        assert!(values.iter().all(Finding::is_violation));
        assert!(values.iter().all(|finding| !finding.related.is_empty()));
    }

    #[test]
    fn platform_command_rule_resolves_qualified_grouped_and_renamed_imports() {
        let values = findings(
            r#"
use std::process::Command;
use std::process as host_process;
use tokio::{process::Command as AsyncCommand};
fn commands() {
    let _ = Command::new("git");
    let _ = host_process::Command::new("git");
    let _ = AsyncCommand::new("git");
    let _ = std::process::Command::new("git");
    let _ = tokio::process::Command::new("git");
}
"#,
            "platform-command-boundary",
        );
        assert_eq!(values.len(), 5);
        assert!(values.iter().all(Finding::is_violation));
        assert!(values
            .iter()
            .all(|finding| finding.message.contains("outside an application")));
    }

    #[test]
    fn platform_command_rule_distinguishes_guest_process_models() {
        let values = findings(
            r#"
struct Process;
impl Process { fn new(_: &str) -> Self { Self } }
fn guest() {
    let _ = Process::new("/bin/sh");
    let _ = hl_engine::Process::new("/bin/bash");
}
"#,
            "platform-command-boundary",
        );
        assert!(values.is_empty());
    }

    #[test]
    fn platform_command_rule_allows_explicit_adapter_modules() {
        let values = findings(
            r#"
mod adapters {
    use std::process::Command as HostCommand;
    fn run() { let _ = HostCommand::new("git"); }
}
mod model {
    fn run() { let _ = std::process::Command::new("git"); }
}
"#,
            "platform-command-boundary",
        );
        assert_eq!(values.len(), 1);
        assert!(values[0].location.source.contains("std::process::Command"));
    }

    #[test]
    fn platform_command_rule_rejects_interpolated_shell_source() {
        let values = findings(
            r#"
fn unsafe_shell(value: &str) {
    let _ = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("echo {value}"));
    let _ = tokio::process::Command::new("bash")
        .args(["-c", value]);
}
"#,
            "platform-command-boundary",
        );
        assert_eq!(values.len(), 4);
        assert_eq!(
            values
                .iter()
                .filter(|finding| finding.subject.contains("interpolated script"))
                .count(),
            2
        );
    }

    #[test]
    fn platform_command_rule_allows_build_commands_but_not_dynamic_shells() {
        let root = temporary("platform-build");
        let path = root.join("build.rs");
        fs::write(
            &path,
            r#"
fn main() {
    let _ = std::process::Command::new("cc").arg("--version").status();
    let _ = std::process::Command::new("sh")
        .arg("-c")
        .arg(concat!("echo ", "static"))
        .status();
    let value = "untrusted";
    let _ = std::process::Command::new("sh").arg("-c").arg(value).status();
}
"#,
        )
        .unwrap();
        let workspace = Workspace::load([path]).unwrap();
        let values = PlatformCommand.check(&workspace).unwrap();
        assert_eq!(values.len(), 1);
        assert!(values[0].subject.contains("interpolated script"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn platform_command_rule_allows_cfg_test_commands() {
        let values = findings(
            r#"
#[cfg(test)]
mod tests {
    fn fixture() { let _ = std::process::Command::new("git"); }
}
#[test]
fn test_fixture() { let _ = tokio::process::Command::new("git"); }
"#,
            "platform-command-boundary",
        );
        assert!(values.is_empty());
    }

    #[test]
    fn platform_command_rule_tracks_staged_shell_builders_with_lexical_scope() {
        let root = temporary("platform-staged");
        let path = root.join("build.rs");
        fs::write(
            &path,
            r#"
use std::process::Command as HostCommand;

fn unsafe_build(value: &str) {
    let mut command = HostCommand::new("/bin/sh");
    command.arg("-c");
    command.arg(format!("echo {value}"));

    let mut alias = command;
    alias.args(["-c", value]);
}

fn safe_build() {
    let mut command = HostCommand::new("sh");
    command.arg("-c");
    command.arg(concat!("echo ", "static"));
}

fn unrelated(value: &str) {
    struct Builder;
    impl Builder { fn arg(&mut self, _: &str) {} }
    let mut command = Builder;
    command.arg("-c");
    command.arg(value);
}
"#,
        )
        .unwrap();
        let workspace = Workspace::load([path]).unwrap();
        let values = PlatformCommand.check(&workspace).unwrap();
        assert_eq!(values.len(), 2);
        assert!(values
            .iter()
            .all(|finding| finding.message.contains("staged shell")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn single_use_rule_excludes_supported_boundaries() {
        let values = findings(
            r#"
fn main() { once(); visual(); public(); ffi(); twice(); twice(); }
fn once() {}
pub fn public() {}
extern "C" fn ffi() {}
// hl-lint: visual-section
fn visual() {}
fn twice() {}
#[test] fn test_only() {}
#[cfg(test)] mod tests { fn fixture() {} }
"#,
            "single-use-free-function",
        );
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].subject, "once");
        assert_eq!(values[0].related.len(), 1);
    }

    #[test]
    fn nesting_rule_counts_structural_depth_but_not_else_if_as_two_levels() {
        let values = findings(
            r#"
fn shallow(value: bool) {
    if value {} else if value {} else if value {}
}
fn deep(value: bool) {
    if value { for _ in 0..1 { match value { true => {}, false => {} } } }
}
"#,
            "deep-control-flow",
        );
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].subject, "deep");
    }

    #[test]
    fn duplicate_entity_rule_requires_identity_relation_and_three_typed_fields() {
        let values = findings(
            r#"
struct Image { id: u64, name: String, path: String }
struct DiscoveredImage { id: u64, name: String, path: String, score: u8 }
struct Unrelated { id: u64, name: String, path: String }
struct WrongTypes { id: String, name: String, path: String }
"#,
            "duplicate-entity-base",
        );
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].subject, "Image_DiscoveredImage");
        assert_eq!(values[0].related.len(), 1);
    }

    #[test]
    fn naming_rule_checks_structs_but_allows_other_types_and_methods() {
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
    fn receiver_repetition_handles_traits_acronyms_versions_and_exclusions() {
        let values = findings(
            r#"
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
"#,
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
        assert_eq!(
            values[3].review.as_ref().unwrap().metadata[1].1,
            "http, server, v, 2"
        );
    }

    #[test]
    fn catch_all_module_rule_checks_rust_module_identity_only() {
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
                review.metadata.iter().any(|(key, value)| {
                    key == "declaration" && value == "inline module declaration"
                })
            })
        }));
    }

    #[test]
    fn catch_all_module_rule_maps_files_directories_and_external_modules() {
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
    fn catch_all_module_rule_does_not_exempt_test_support() {
        let root = temporary("catch-all-tests");
        let tests = root.join("tests");
        fs::create_dir_all(tests.join("common")).unwrap();
        fs::write(tests.join("common/mod.rs"), "pub struct Harness;\n").unwrap();
        fs::write(tests.join("workflow.rs"), "mod common;\n").unwrap();

        let workspace = Workspace::load([tests]).unwrap();
        let values = CatchAllModule.check(&workspace).unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].subject, "common");
        assert!(values[0]
            .review
            .as_ref()
            .unwrap()
            .metadata
            .contains(&("scope".to_owned(), "test".to_owned())));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reporters_render_to_injected_outputs() {
        let root = temporary("reporters");
        let source = write(
            &root,
            "#[hl_design::classify(pkg)] fn reviewed(value: usize) { let _ = value; }\nfn missing(value: usize) { reviewed(value); }",
        );
        let mut diagnostic = Diagnostic::new(Vec::new());
        Linter::standard()
            .run([source.clone()], &mut diagnostic)
            .unwrap();
        let diagnostic = String::from_utf8(diagnostic.into_inner()).unwrap();
        assert!(diagnostic.contains("unclassified free function `missing`"));
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
    fn file_length_rule_includes_integration_tests() {
        let root = temporary("test-file-length");
        let tests = root.join("tests");
        fs::create_dir_all(&tests).unwrap();
        let path = tests.join("oversized.rs");
        fs::write(&path, format!("{}struct Fixture;\n", "\n".repeat(500))).unwrap();

        let workspace = Workspace::load([path]).unwrap();
        let findings = FileLength.check(&workspace).unwrap();

        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("501 lines"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_directory_rule_reports_empty_and_placeholder_only_folders() {
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
            [
                Path::new("src/adapter/planned"),
                Path::new("src/model/unused"),
            ]
        );
        assert!(findings.iter().all(Finding::is_violation));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_directory_rule_ignores_generated_and_excluded_trees() {
        let root = temporary("empty-directory-exclusions");
        for path in [
            "lint/errors",
            "target/debug/empty",
            "vendor/package/empty",
            "src/engine/hl-engine/empty",
            "src/packages/hl-design-lint/empty",
        ] {
            fs::create_dir_all(root.join(path)).unwrap();
        }
        write(&root, "pub struct Present;\n");

        let workspace = Workspace::load([root.clone()]).unwrap();

        assert!(EmptyDirectory.check(&workspace).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workspace_excludes_the_linter_even_when_addressed_directly() {
        let root = temporary("self-exclusion");
        let linter = root.join("hl-design-lint");
        fs::create_dir_all(linter.join("src")).unwrap();
        fs::write(
            linter.join("src/lib.rs"),
            "fn would_otherwise_be_reported(value: usize) { let _ = value; }",
        )
        .unwrap();
        let workspace = Workspace::load([linter]).unwrap();
        assert!(workspace.sources().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workspace_excludes_only_the_paused_engine_domain() {
        let root = temporary("paused-domains");
        for (domain, package, source) in [
            ("engine", "hl-engine", "fn engine(value: u8) {}\n"),
            ("containers", "hl-daemon", "fn container(value: u8) {}\n"),
            ("gpu", "hl-gpu", "fn gpu(value: u8) {}\n"),
        ] {
            let directory = root.join(format!("src/{domain}/{package}/src"));
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("lib.rs"), source).unwrap();
        }

        let workspace = Workspace::load([root.join("src")]).expect("load workspace");
        let paths = workspace
            .sources()
            .iter()
            .map(|source| source.path.as_path())
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            [
                root.join("src/containers/hl-daemon/src/lib.rs").as_path(),
                root.join("src/gpu/hl-gpu/src/lib.rs").as_path(),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }
}
