use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{policy::SourcePolicy, source::source_files_with_policy};

const TIDY_CHECKS: &str = "clang-analyzer-*,-clang-analyzer-security.insecureAPI.DeprecatedOrUnsafeBufferHandling,-clang-analyzer-security.MmapWriteExec,-clang-analyzer-unix.BlockInCriticalSection,bugprone-assignment-in-if-condition,bugprone-branch-clone,bugprone-inc-dec-in-conditions,bugprone-infinite-loop,bugprone-not-null-terminated-result,bugprone-posix-return,bugprone-signal-handler,bugprone-sizeof-expression,bugprone-suspicious-memory-comparison,bugprone-suspicious-memset-usage,bugprone-undefined-memory-manipulation";

#[derive(Clone, Debug)]
/// Executables and compilation database used by the external C analyzers.
pub struct AnalyzerConfig {
    /// `clang-format` executable.
    pub clang_format: PathBuf,
    /// `clang-tidy` executable.
    pub clang_tidy: PathBuf,
    /// `cppcheck` executable.
    pub cppcheck: PathBuf,
    /// Directory containing `compile_commands.json`.
    pub compilation_database: PathBuf,
}

#[derive(Clone, Deserialize, Serialize)]
struct Compilation {
    directory: PathBuf,
    file: PathBuf,
    #[serde(flatten)]
    metadata: BTreeMap<String, Value>,
}

/// Runs the Nix-provided C analyzers over arbitrary source roots.
///
/// Tool output is forwarded verbatim because clang and cppcheck already emit
/// file/line/column diagnostics understood by editors and CI annotation tools.
pub fn run(config: &AnalyzerConfig, roots: &[PathBuf], policy: &SourcePolicy) -> Result<bool, String> {
    let files = source_files(roots, policy)?;
    let mut clean = true;
    for file in &files {
        clean &= invoke(
            "clang-format",
            Command::new(&config.clang_format)
                .args(["--dry-run", "--Werror", "--style=file", "--ferror-limit=1"])
                .arg(file),
            Some(file),
        )?;
    }

    let database = config.compilation_database.join("compile_commands.json");
    let translation_units = translation_units(&database, &files)?;
    let filtered_database = filtered_database(&database, &translation_units)?;
    for file in translation_units {
        clean &= invoke(
            "clang-tidy",
            Command::new(&config.clang_tidy)
                .args(["--quiet", "-p"])
                .arg(filtered_database.path())
                .arg(format!("--checks={TIDY_CHECKS}"))
                .args(["--extra-arg=-std=c11", "--warnings-as-errors=*"])
                .arg(&file),
            Some(&file),
        )?;
    }

    clean &= invoke(
        "cppcheck",
        Command::new(&config.cppcheck)
            .args([
                "--quiet",
                "--std=c11",
                "--enable=warning,performance,portability",
                "--inconclusive",
                "--suppress=missingIncludeSystem",
                "--suppress=unmatchedSuppression",
                "--suppress=unusedStructMember",
                "--suppress=constParameter",
                "--suppress=normalCheckLevelMaxBranches",
                "--suppress=toomanyconfigs",
                "--suppress=preprocessorErrorDirective",
                "--error-exitcode=1",
            ])
            .arg(format!(
                "--project={}",
                filtered_database.path().join("compile_commands.json").display()
            )),
        Some(&filtered_database.path().join("compile_commands.json")),
    )?;
    Ok(clean)
}

fn invoke(label: &str, command: &mut Command, subject: Option<&Path>) -> Result<bool, String> {
    let output = command
        .output()
        .map_err(|error| format!("execute {}: {error}", command.get_program().to_string_lossy()))?;
    forward(&output);
    if output.status.success() {
        return Ok(true);
    }
    let subject = subject.map_or_else(|| Path::new(".").to_path_buf(), Path::to_path_buf);
    eprintln!("{}:1:1: error: {label} reported diagnostics", subject.display());
    Ok(false)
}

fn forward(output: &Output) {
    use std::io::Write as _;
    let _ = std::io::stdout().write_all(&output.stdout);
    let _ = std::io::stderr().write_all(&output.stderr);
}

fn source_files(roots: &[PathBuf], policy: &SourcePolicy) -> Result<Vec<PathBuf>, String> {
    let files = source_files_with_policy(roots, policy)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|path| matches!(path.extension().and_then(|value| value.to_str()), Some("c" | "h")))
        .collect::<Vec<_>>();
    if files.is_empty() {
        return Err("requested analyzer roots contain no C source or header".into());
    }
    Ok(files)
}

fn translation_units(database: &Path, allowed_sources: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let bytes = fs::read(database).map_err(|error| format!("read {}: {error}", database.display()))?;
    let commands: Vec<Compilation> =
        serde_json::from_slice(&bytes).map_err(|error| format!("decode {}: {error}", database.display()))?;
    let allowed_sources = allowed_sources
        .iter()
        .filter(|source| source.extension().and_then(|value| value.to_str()) == Some("c"))
        .map(|source| {
            source
                .canonicalize()
                .map_err(|error| format!("resolve {}: {error}", source.display()))
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let mut files = BTreeSet::new();
    for command in commands {
        let file = if command.file.is_absolute() {
            command.file
        } else {
            let directory = command
                .directory
                .canonicalize()
                .map_err(|error| format!("resolve compilation directory {}: {error}", command.directory.display()))?;
            directory.join(command.file)
        };
        let Ok(file) = file.canonicalize() else {
            continue;
        };
        if allowed_sources.contains(&file) {
            files.insert(file);
        }
    }
    if files.is_empty() && !allowed_sources.is_empty() {
        return Err(format!(
            "{} contains no policy-allowed C translation unit from the requested source roots",
            database.display()
        ));
    }
    Ok(files.into_iter().collect())
}

fn filtered_database(database: &Path, translation_units: &[PathBuf]) -> Result<tempfile::TempDir, String> {
    let bytes = fs::read(database).map_err(|error| format!("read {}: {error}", database.display()))?;
    let commands: Vec<Compilation> =
        serde_json::from_slice(&bytes).map_err(|error| format!("decode {}: {error}", database.display()))?;
    let requested = translation_units.iter().collect::<BTreeSet<_>>();
    let commands = commands
        .into_iter()
        .filter(|command| {
            let file = if command.file.is_absolute() {
                command.file.clone()
            } else {
                command.directory.join(&command.file)
            };
            file.canonicalize().is_ok_and(|file| requested.contains(&file))
        })
        .collect::<Vec<_>>();
    let directory = tempfile::Builder::new()
        .prefix("hl-design-lint-c-database-")
        .tempdir()
        .map_err(|error| format!("create filtered compilation database: {error}"))?;
    let output = directory.path().join("compile_commands.json");
    let bytes = serde_json::to_vec(&commands).map_err(|error| format!("encode {}: {error}", output.display()))?;
    fs::write(&output, bytes).map_err(|error| format!("write {}: {error}", output.display()))?;
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::{Compilation, filtered_database, source_files, translation_units};
    use crate::policy::SourcePolicy;
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    fn fixture(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("c-analyzer-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        root
    }

    fn allowed(root: &Path) -> Vec<PathBuf> {
        source_files(&[root.join("src")], &SourcePolicy::default()).unwrap()
    }

    #[test]
    fn discovers_only_c_sources_and_headers() {
        let root = fixture("sources");
        fs::write(root.join("src/a.c"), "int a;\n").unwrap();
        fs::write(root.join("src/a.h"), "int a;\n").unwrap();
        fs::write(root.join("src/a.rs"), "fn a() {}\n").unwrap();
        let files = source_files(&[root.join("src")], &SourcePolicy::default()).unwrap();
        assert_eq!(files, [root.join("src/a.c"), root.join("src/a.h")]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_vacuous_analyzer_roots() {
        let root = fixture("no-c-sources");
        fs::write(root.join("src/lib.rs"), "pub fn value() -> usize { 1 }\n").unwrap();
        let error = source_files(&[root.join("src")], &SourcePolicy::default()).unwrap_err();
        assert!(error.contains("no C source or header"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selects_only_compiled_units_below_requested_roots() {
        let root = fixture("database");
        fs::write(root.join("src/a.c"), "int a;\n").unwrap();
        fs::write(root.join("outside.c"), "int b;\n").unwrap();
        fs::write(
            root.join("compile_commands.json"),
            format!(
                "[{{\"directory\":{0:?},\"file\":\"src/a.c\"}},{{\"directory\":{0:?},\"file\":\"outside.c\"}}]",
                root.to_string_lossy()
            ),
        )
        .unwrap();
        let files = translation_units(&root.join("compile_commands.json"), &allowed(&root)).unwrap();
        assert_eq!(files, [root.join("src/a.c").canonicalize().unwrap()]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_database_that_would_skip_every_requested_translation_unit() {
        let root = fixture("missing-database-coverage");
        fs::write(root.join("src/a.c"), "int a;\n").unwrap();
        fs::write(root.join("outside.c"), "int b;\n").unwrap();
        fs::write(
            root.join("compile_commands.json"),
            format!(
                "[{{\"directory\":{0:?},\"file\":\"outside.c\"}}]",
                root.to_string_lossy()
            ),
        )
        .unwrap();
        let error = translation_units(&root.join("compile_commands.json"), &allowed(&root)).unwrap_err();
        assert!(error.contains("no policy-allowed C translation unit"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn permits_header_only_analyzer_roots() {
        let root = fixture("header-only");
        fs::write(root.join("src/api.h"), "int api(void);\n").unwrap();
        fs::write(root.join("compile_commands.json"), "[]").unwrap();
        let files = translation_units(&root.join("compile_commands.json"), &allowed(&root)).unwrap();
        assert!(files.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ignores_missing_compiled_unit_outside_policy_allowlist() {
        let root = fixture("missing-compiled-unit");
        fs::write(root.join("src/a.c"), "int a;\n").unwrap();
        fs::write(
            root.join("compile_commands.json"),
            format!(
                "[{{\"directory\":{0:?},\"file\":\"src/a.c\"}},\
                  {{\"directory\":{0:?},\"file\":\"src/missing.c\"}}]",
                root.to_string_lossy()
            ),
        )
        .unwrap();
        let files = translation_units(&root.join("compile_commands.json"), &allowed(&root)).unwrap();
        assert_eq!(files, [root.join("src/a.c").canonicalize().unwrap()]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn filtered_database_excludes_unrequested_and_generated_commands() {
        let root = fixture("filtered-database");
        fs::write(root.join("src/a.c"), "int a;\n").unwrap();
        fs::write(root.join("outside.c"), "int b;\n").unwrap();
        fs::write(
            root.join("compile_commands.json"),
            format!(
                "[{{\"directory\":{0:?},\"file\":\"src/a.c\",\"arguments\":[\"cc\",\"-c\",\"src/a.c\"],\"output\":\"a.o\"}},\
                  {{\"directory\":{0:?},\"file\":\"outside.c\",\"command\":\"cc -c outside.c\"}},\
                  {{\"directory\":{0:?},\"file\":\"target/generated.c\",\"command\":\"cc -c target/generated.c\"}}]",
                root.to_string_lossy()
            ),
        )
        .unwrap();
        let units = translation_units(&root.join("compile_commands.json"), &allowed(&root)).unwrap();
        let database = filtered_database(&root.join("compile_commands.json"), &units).unwrap();
        let commands: Vec<Compilation> =
            serde_json::from_slice(&fs::read(database.path().join("compile_commands.json")).unwrap()).unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].file, PathBuf::from("src/a.c"));
        assert_eq!(commands[0].metadata["output"], "a.o");
        assert_eq!(commands[0].metadata["arguments"][0], "cc");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_policy_excludes_named_and_marker_owned_directories_in_custom_layouts() {
        let root = fixture("source-policy");
        let native = root.join("components/runtime/native-code");
        fs::create_dir_all(native.join("generated")).unwrap();
        fs::create_dir_all(native.join("external/nested")).unwrap();
        fs::write(native.join("engine.c"), "int engine(void) { return 0; }\n").unwrap();
        fs::write(native.join("api.h"), "int engine(void);\n").unwrap();
        fs::write(native.join("generated/bypass.c"), "int bypass(void);\n").unwrap();
        fs::write(native.join("external/.external-source"), "").unwrap();
        fs::write(native.join("external/nested/bypass.c"), "int bypass(void);\n").unwrap();
        let policy = SourcePolicy {
            ignored_directories: vec!["generated".into()],
            ignored_markers: vec![".external-source".into()],
            self_packages: Vec::new(),
            foreign_source_directories: Vec::new(),
        };

        let files = source_files(std::slice::from_ref(&native), &policy).unwrap();
        assert_eq!(files, [native.join("api.h"), native.join("engine.c")]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn compile_database_cannot_reintroduce_policy_ignored_sources() {
        let root = fixture("database-policy");
        fs::create_dir_all(root.join("src/generated")).unwrap();
        fs::create_dir_all(root.join("src/external")).unwrap();
        fs::write(root.join("src/allowed.c"), "int allowed(void);\n").unwrap();
        fs::write(root.join("src/generated/bypass.c"), "int bypass(void);\n").unwrap();
        fs::write(root.join("src/external/.external-source"), "").unwrap();
        fs::write(root.join("src/external/bypass.c"), "int bypass(void);\n").unwrap();
        fs::write(
            root.join("compile_commands.json"),
            format!(
                "[{{\"directory\":{0:?},\"file\":\"src/allowed.c\"}},\
                  {{\"directory\":{0:?},\"file\":\"src/generated/bypass.c\"}},\
                  {{\"directory\":{0:?},\"file\":\"src/external/bypass.c\"}}]",
                root.to_string_lossy()
            ),
        )
        .unwrap();
        let policy = SourcePolicy {
            ignored_directories: vec!["generated".into()],
            ignored_markers: vec![".external-source".into()],
            self_packages: Vec::new(),
            foreign_source_directories: Vec::new(),
        };
        let allowed = source_files(&[root.join("src")], &policy).unwrap();

        let files = translation_units(&root.join("compile_commands.json"), &allowed).unwrap();
        assert_eq!(files, [root.join("src/allowed.c").canonicalize().unwrap()]);
        fs::remove_dir_all(root).unwrap();
    }
}
