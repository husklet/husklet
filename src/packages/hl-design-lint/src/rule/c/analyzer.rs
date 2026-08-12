use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde::Deserialize;

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

#[derive(Deserialize)]
struct Compilation {
    directory: PathBuf,
    file: PathBuf,
}

/// Runs the Nix-provided C analyzers over arbitrary source roots.
///
/// Tool output is forwarded verbatim because clang and cppcheck already emit
/// file/line/column diagnostics understood by editors and CI annotation tools.
pub fn run(config: &AnalyzerConfig, roots: &[PathBuf]) -> Result<bool, String> {
    let files = source_files(roots)?;
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
    let translation_units = translation_units(&database, roots)?;
    for file in translation_units {
        clean &= invoke(
            "clang-tidy",
            Command::new(&config.clang_tidy)
                .args(["--quiet", "-p"])
                .arg(&config.compilation_database)
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
            .arg(format!("--project={}", database.display())),
        Some(&database),
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

fn source_files(roots: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut files = BTreeSet::new();
    for root in roots {
        collect(root, &mut files)?;
    }
    Ok(files.into_iter().collect())
}

fn collect(path: &Path, files: &mut BTreeSet<PathBuf>) -> Result<(), String> {
    if path.is_dir() {
        let mut entries = fs::read_dir(path)
            .map_err(|error| format!("read {}: {error}", path.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("enumerate {}: {error}", path.display()))?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            collect(&entry.path(), files)?;
        }
    } else if matches!(path.extension().and_then(|value| value.to_str()), Some("c" | "h")) {
        files.insert(path.to_path_buf());
    }
    Ok(())
}

fn translation_units(database: &Path, roots: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let bytes = fs::read(database).map_err(|error| format!("read {}: {error}", database.display()))?;
    let commands: Vec<Compilation> =
        serde_json::from_slice(&bytes).map_err(|error| format!("decode {}: {error}", database.display()))?;
    let roots = roots
        .iter()
        .map(|root| {
            root.canonicalize()
                .map_err(|error| format!("resolve {}: {error}", root.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut files = BTreeSet::new();
    for command in commands {
        let file = if command.file.is_absolute() {
            command.file
        } else {
            command.directory.join(command.file)
        };
        let Ok(file) = file.canonicalize() else { continue };
        if file.extension().and_then(|value| value.to_str()) == Some("c")
            && roots.iter().any(|root| file.starts_with(root))
        {
            files.insert(file);
        }
    }
    Ok(files.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::{source_files, translation_units};
    use std::{fs, path::PathBuf};

    fn fixture(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("c-analyzer-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        root
    }

    #[test]
    fn discovers_only_c_sources_and_headers() {
        let root = fixture("sources");
        fs::write(root.join("src/a.c"), "int a;\n").unwrap();
        fs::write(root.join("src/a.h"), "int a;\n").unwrap();
        fs::write(root.join("src/a.rs"), "fn a() {}\n").unwrap();
        let files = source_files(&[root.join("src")]).unwrap();
        assert_eq!(files, [root.join("src/a.c"), root.join("src/a.h")]);
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
        let files = translation_units(&root.join("compile_commands.json"), &[root.join("src")]).unwrap();
        assert_eq!(files, [root.join("src/a.c").canonicalize().unwrap()]);
        fs::remove_dir_all(root).unwrap();
    }
}
