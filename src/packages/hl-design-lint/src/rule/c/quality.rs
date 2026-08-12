use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::rule::Rule;
use crate::{Finding, LintError, Location, Result, Severity, source::Workspace};

const RULE: &str = "native-quality";
const EXTENSIONS: &[&str] = &["c", "h", "m", "mm", "S"];
const STDIO_ALLOW: &[&str] = &[
    "retained/src/core/target/aarch64.c",
    "retained/src/core/target/x86_64.c",
    "retained/src/linux_abi/checkpoint.c",
    "retained/src/linux_abi/container/netns.c",
    "retained/src/linux_abi/container/state.c",
    "retained/src/linux_abi/elf.c",
    "retained/src/linux_abi/fork.c",
    "retained/src/linux_abi/parse.c",
    "retained/src/linux_abi/sentry.c",
    "retained/src/linux_abi/syscall/dispatch.c",
    "retained/src/linux_abi/syscall/event.c",
    "retained/src/linux_abi/syscall/inotify.c",
    "retained/src/linux_abi/syscall/io.c",
    "retained/src/linux_abi/syscall/proc.c",
    "retained/src/linux_abi/x86.c",
    "retained/src/translator/guest/aarch64/cache.c",
    "retained/src/translator/guest/aarch64/signal.c",
    "retained/src/translator/guest/aarch64/translate.c",
    "retained/src/translator/guest/x86_64/avx.c",
    "retained/src/translator/guest/x86_64/cache.c",
    "retained/src/translator/guest/x86_64/dispatch.h",
    "retained/src/translator/guest/x86_64/signal.c",
    "retained/src/translator/guest/x86_64/translate.c",
    "retained/tools/lifecycle_runner.c",
];

/// Repository-owned C source inventory, deterministic policies, and external analyzers.
pub struct Quality {
    compile_commands: PathBuf,
}

impl Quality {
    /// Creates the native quality rule for a configured CMake build directory.
    #[must_use]
    pub fn new(compile_commands: PathBuf) -> Self {
        Self { compile_commands }
    }
}

impl Rule for Quality {
    fn id(&self) -> &'static str {
        RULE
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, _workspace: &Workspace) -> Result<Vec<Finding>> {
        let root = PathBuf::from("src/runtime/native");
        let files = inventory(&root)?;
        let mut findings = inventory_findings(&root, &files)?;
        let sources = source_paths(&root)?;
        for file in &sources {
            if file.extension().and_then(|value| value.to_str()) != Some("S") {
                findings.extend(policy_findings(&root, file)?);
            }
        }
        if findings.is_empty() {
            run_analyzers(&sources, &self.compile_commands)?;
        }
        Ok(findings)
    }
}

fn source_paths(root: &Path) -> Result<Vec<PathBuf>> {
    let manifest = root.join("lint-sources.manifest");
    let text = fs::read_to_string(&manifest).map_err(|error| LintError::io("read", &manifest, error))?;
    Ok(text
        .lines()
        .filter_map(|line| line.strip_prefix("source\t"))
        .map(|path| root.join(path))
        .collect())
}

fn inventory(root: &Path) -> Result<Vec<PathBuf>> {
    fn walk(path: &Path, output: &mut Vec<PathBuf>) -> std::io::Result<()> {
        if path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.starts_with('.'))
        {
            return Ok(());
        }
        if path.is_dir() {
            for entry in fs::read_dir(path)? {
                walk(&entry?.path(), output)?;
            }
        } else if path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|ext| EXTENSIONS.contains(&ext))
        {
            output.push(path.to_path_buf());
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(root, &mut files).map_err(|error| LintError::io("walk", root, error))?;
    files.sort();
    Ok(files)
}

fn inventory_findings(root: &Path, files: &[PathBuf]) -> Result<Vec<Finding>> {
    let manifest = root.join("lint-sources.manifest");
    let text = fs::read_to_string(&manifest).map_err(|error| LintError::io("read", &manifest, error))?;
    let actual = files
        .iter()
        .filter_map(|path| path.strip_prefix(root).ok())
        .map(Path::to_path_buf)
        .collect::<BTreeSet<_>>();
    let mut expected = BTreeSet::new();
    let mut findings = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((class, path)) = line.split_once('\t') else {
            findings.push(finding(
                &manifest,
                index + 1,
                line,
                "invalid inventory row",
                "use `<source|fixture|assembly>\\t<path>`",
            ));
            continue;
        };
        if !matches!(class, "source" | "fixture" | "assembly") || (class == "assembly") != path.ends_with(".S") {
            findings.push(finding(
                &manifest,
                index + 1,
                line,
                "invalid source classification",
                "classify .S as assembly and C/H files as source or fixture",
            ));
        }
        expected.insert(PathBuf::from(path));
    }
    for path in actual.difference(&expected) {
        findings.push(finding(
            &root.join(path),
            1,
            "",
            "source is absent from native inventory",
            "add a classified row to lint-sources.manifest",
        ));
    }
    for path in expected.difference(&actual) {
        findings.push(finding(
            &manifest,
            1,
            &path.display().to_string(),
            "stale native inventory entry",
            "remove the row or restore the source",
        ));
    }
    Ok(findings)
}

fn policy_findings(root: &Path, path: &Path) -> Result<Vec<Finding>> {
    let text = fs::read_to_string(path).map_err(|error| LintError::io("read", path, error))?;
    let relative = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
    let stdio_allowed = STDIO_ALLOW.contains(&relative.as_ref());
    let mut findings = Vec::new();
    let mut block = false;
    for (index, raw) in text.lines().enumerate() {
        let clean = sanitize(raw, &mut block);
        let calls = |names: &[&str]| names.iter().any(|name| has_call(&clean, name));
        if calls(&[
            "getenv",
            "secure_getenv",
            "__secure_getenv",
            "_dupenv_s",
            "GetEnvironmentVariableA",
            "GetEnvironmentVariableW",
        ]) {
            findings.push(finding(
                path,
                index + 1,
                raw,
                "direct environment access",
                "read configuration through the engine option boundary",
            ));
        }
        let console = calls(&["printf", "vprintf", "puts", "putchar", "perror", "dprintf", "vdprintf"])
            || (calls(&["fprintf", "vfprintf", "fputs", "fputc"])
                && (clean.contains("stderr") || clean.contains("stdout")));
        if !stdio_allowed
            && (console || calls(&["OutputDebugStringA", "OutputDebugStringW", "NSLog", "os_log", "syslog"]))
        {
            findings.push(finding(
                path,
                index + 1,
                raw,
                "direct diagnostic output",
                "emit through tagged logging",
            ));
        }
        if calls(&["system", "popen"]) {
            findings.push(finding(
                path,
                index + 1,
                raw,
                "shell execution",
                "launch an explicit argv vector",
            ));
        }
    }
    Ok(findings)
}

fn sanitize(line: &str, block: &mut bool) -> String {
    let mut output = String::new();
    let bytes = line.as_bytes();
    let mut index = 0;
    let mut quote = None;
    while index < bytes.len() {
        if *block {
            if index + 1 < bytes.len() && &bytes[index..index + 2] == b"*/" {
                *block = false;
                index += 2;
            } else {
                index += 1;
            }
        } else if quote.is_none() && index + 1 < bytes.len() && &bytes[index..index + 2] == b"//" {
            break;
        } else if quote.is_none() && index + 1 < bytes.len() && &bytes[index..index + 2] == b"/*" {
            *block = true;
            index += 2;
        } else if let Some(mark) = quote {
            if bytes[index] == b'\\' {
                index += 2;
            } else {
                if bytes[index] == mark {
                    quote = None;
                }
                index += 1;
            }
        } else if matches!(bytes[index], b'"' | b'\'') {
            quote = Some(bytes[index]);
            index += 1;
        } else {
            output.push(bytes[index] as char);
            index += 1;
        }
    }
    output
}

fn has_call(line: &str, name: &str) -> bool {
    line.match_indices(name).any(|(index, _)| {
        let left =
            index == 0 || !line.as_bytes()[index - 1].is_ascii_alphanumeric() && line.as_bytes()[index - 1] != b'_';
        let rest = &line[index + name.len()..];
        let right = rest
            .as_bytes()
            .first()
            .map_or(true, |byte| !byte.is_ascii_alphanumeric() && *byte != b'_');
        left && right && rest.trim_start().starts_with('(')
    })
}

fn run_analyzers(files: &[PathBuf], compile_commands: &Path) -> Result<()> {
    let database = compile_commands.join("compile_commands.json");
    if !database.is_file() {
        return Err(LintError::io(
            "read",
            compile_commands,
            std::io::Error::new(std::io::ErrorKind::NotFound, "compile_commands.json is missing"),
        ));
    }
    for path in files
        .iter()
        .filter(|path| path.extension().and_then(|value| value.to_str()) != Some("S"))
    {
        run(
            "clang-format",
            &["--dry-run", "--Werror", "--style=file", &path.to_string_lossy()],
        )?;
    }
    run(
        "cppcheck",
        &[
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
            &format!("--project={}", compile_commands.join("compile_commands.json").display()),
        ],
    )?;
    let text = fs::read_to_string(&database).map_err(|error| LintError::io("read", &database, error))?;
    let rows: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
        LintError::io(
            "parse",
            &database,
            std::io::Error::new(std::io::ErrorKind::InvalidData, error),
        )
    })?;
    let mut translation_units = BTreeSet::new();
    for row in rows.as_array().into_iter().flatten() {
        if let Some(file) = row
            .get("file")
            .and_then(serde_json::Value::as_str)
            .filter(|file| file.ends_with(".c"))
        {
            translation_units.insert(file.to_owned());
        }
    }
    for file in translation_units {
        run(
            "clang-tidy",
            &[
                "--quiet",
                "-p",
                &compile_commands.to_string_lossy(),
                "--warnings-as-errors=*",
                &file,
            ],
        )?;
    }
    Ok(())
}

fn run(program: &str, arguments: &[&str]) -> Result<()> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| LintError::io("execute", Path::new(program), error))?;
    if output.status.success() {
        return Ok(());
    }
    Err(LintError::io(
        "execute",
        Path::new(program),
        std::io::Error::other(String::from_utf8_lossy(&output.stderr).into_owned()),
    ))
}

fn finding(path: &Path, line: usize, source: &str, message: &str, help: &str) -> Finding {
    let mut finding = Finding::error(
        RULE,
        message,
        Location {
            path: path.to_path_buf(),
            line,
            column: 1,
            source: source.to_owned(),
        },
    );
    finding.message = message.to_owned();
    finding.help = help.to_owned();
    finding
}

#[cfg(test)]
#[path = "quality_test.rs"]
mod test;
