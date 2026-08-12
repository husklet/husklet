use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use super::super::Rule;
use crate::{DocumentationPolicy, Finding, LintError, Location, Result, Severity, source::Workspace};

const RULE: &str = "documentation-contract";

/// Enforces a configured Markdown inventory and generic example-document shape.
pub struct Documentation {
    policy: DocumentationPolicy,
    ignored_directories: BTreeSet<String>,
}

impl Documentation {
    /// Creates the rule from repository-owned paths and traversal exclusions.
    #[must_use]
    pub fn new(policy: DocumentationPolicy, ignored_directories: Vec<String>) -> Self {
        Self {
            policy,
            ignored_directories: ignored_directories.into_iter().collect(),
        }
    }
}

impl Rule for Documentation {
    fn id(&self) -> &'static str {
        RULE
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, _workspace: &Workspace) -> Result<Vec<Finding>> {
        if self.policy.allowed.is_empty() && self.policy.examples.is_empty() {
            return Ok(Vec::new());
        }
        let allowed = self.policy.allowed.iter().map(PathBuf::from).collect::<BTreeSet<_>>();
        let mut markdown = Vec::new();
        walk(Path::new("."), &self.ignored_directories, &mut markdown)?;
        let mut findings = Vec::new();
        for path in markdown {
            let normalized = path.strip_prefix(".").unwrap_or(&path).to_path_buf();
            if !allowed.contains(&normalized) {
                findings.push(finding(
                    &normalized,
                    1,
                    "Markdown file is not in the repository allowlist",
                    "remove it or add its intentional role to the root policy",
                ));
            }
        }
        for configured in &self.policy.allowed {
            let path = PathBuf::from(configured);
            if !path.is_file() {
                findings.push(finding(
                    &path,
                    1,
                    "allowed Markdown file is missing",
                    "restore the file or remove its policy entry",
                ));
            }
        }
        for configured in &self.policy.examples {
            let path = PathBuf::from(configured);
            if !allowed.contains(&path) {
                findings.push(finding(
                    &path,
                    1,
                    "example document is not allowed",
                    "include every example document in the Markdown allowlist",
                ));
            } else if path.is_file() {
                validate_example(&path, &mut findings)?;
            }
        }
        Ok(findings)
    }
}

fn walk(path: &Path, ignored: &BTreeSet<String>, output: &mut Vec<PathBuf>) -> Result<()> {
    if path.is_dir() {
        if path != Path::new(".")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| ignored.contains(name))
        {
            return Ok(());
        }
        let entries = fs::read_dir(path).map_err(|error| LintError::io("read directory", path, error))?;
        for entry in entries {
            walk(
                &entry
                    .map_err(|error| LintError::io("read directory", path, error))?
                    .path(),
                ignored,
                output,
            )?;
        }
    } else if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        output.push(path.to_path_buf());
    }
    Ok(())
}

fn validate_example(path: &Path, findings: &mut Vec<Finding>) -> Result<()> {
    let text = fs::read_to_string(path).map_err(|error| LintError::io("read example", path, error))?;
    let headings = text.lines().filter(|line| line.starts_with("# ")).count();
    if !text.starts_with("# ") || headings != 1 {
        findings.push(finding(
            path,
            1,
            "example document needs exactly one leading title",
            "start with one level-one Markdown heading",
        ));
    }
    if !text.lines().any(|line| line.starts_with("## ")) {
        findings.push(finding(
            path,
            1,
            "example document has no cases",
            "add at least one level-two case heading",
        ));
    }
    if text.lines().filter(|line| line.trim_start().starts_with("```")).count() % 2 != 0 {
        findings.push(finding(
            path,
            1,
            "example document has an unclosed code fence",
            "close every fenced example",
        ));
    }
    Ok(())
}

fn finding(path: &Path, line: usize, message: &str, help: &str) -> Finding {
    let mut finding = Finding::error(
        RULE,
        path.display().to_string(),
        Location {
            path: path.to_path_buf(),
            line,
            column: 1,
            source: String::new(),
        },
    );
    finding.message = message.to_owned();
    finding.help = help.to_owned();
    finding
}

#[cfg(test)]
mod test;
