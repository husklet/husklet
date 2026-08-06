use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    LintError, Result,
    model::{Finding, Location, Review, Severity},
    rule::Rule,
    source::{Workspace, domain, package},
};

#[cfg(test)]
#[path = "test.rs"]
mod tests;

/// Rejects relative paths that leave the repository and `include!` sources owned by another directory.
pub struct Repository;

impl Rule for Repository {
    fn id(&self) -> &'static str {
        "repository-escape"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut files = Vec::new();
        for path in workspace.paths() {
            collect(path, &mut files).map_err(|error| LintError::io("walk", path, error))?;
        }
        files.sort();
        files.dedup();
        let mut findings = Vec::new();
        for file in files {
            let text = fs::read_to_string(&file).map_err(|error| LintError::io("read", &file, error))?;
            for (index, line) in text.lines().enumerate() {
                findings.extend(line_findings(self.id(), &file, index + 1, line));
            }
        }
        Ok(findings)
    }
}

fn collect(path: &Path, output: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if path.symlink_metadata()?.file_type().is_symlink() {
        return Ok(());
    }
    if path.is_file() {
        if inspected(path) {
            output.push(path.to_owned());
        }
        return Ok(());
    }
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, ".git" | "target" | "vendor" | "lint" | "hl-design-lint"))
    {
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        collect(&entry?.path(), output)?;
    }
    Ok(())
}

fn inspected(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "Cargo.toml")
        || path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| matches!(value, "rs" | "c" | "h" | "S"))
}

fn line_findings(rule: &'static str, file: &Path, line: usize, text: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    for literal in literals(text) {
        if !literal.contains("..") || literal.starts_with('/') {
            continue;
        }
        let included = ["include!", "include_str!", "include_bytes!"]
            .iter()
            .any(|macro_name| text.contains(macro_name));
        let outside = escapes(repository_depth(file), literal);
        let borrowed = included && escapes(crate_depth(file), literal);
        if !borrowed && !outside {
            continue;
        }
        let message = if outside {
            format!("`{literal}` resolves outside the repository root")
        } else {
            format!("`{literal}` includes source owned by another directory")
        };
        findings.push(finding(rule, file, line, literal, message));
    }
    findings
}

fn finding(rule: &'static str, file: &Path, line: usize, literal: &str, message: String) -> Finding {
    let mut value = Finding::error(
        rule,
        literal.to_owned(),
        Location {
            path: file.to_owned(),
            line,
            column: 1,
            source: literal.to_owned(),
        },
    );
    value.message = message;
    value.help = "depend on a workspace crate that owns the files, or take the path as an explicit argument".to_owned();
    let mut review = Review::error();
    review.metadata.push(("domain".to_owned(), domain(file)));
    review.metadata.push((
        "package".to_owned(),
        package(file).unwrap_or_else(|| "repository".to_owned()),
    ));
    review.questions = vec!["Which workspace crate should own the referenced files?".to_owned()];
    value.review = Some(review);
    value
}

fn literals(text: &str) -> Vec<&str> {
    text.split('"').skip(1).step_by(2).collect()
}

fn repository_depth(file: &Path) -> Option<usize> {
    let components = file
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    let root = components.iter().position(|component| *component == "src")?;
    components.len().checked_sub(root + 1)
}

fn crate_depth(file: &Path) -> Option<usize> {
    let root = file
        .ancestors()
        .skip(1)
        .position(|directory| directory.join("Cargo.toml").is_file())?;
    Some(root)
}

/// Reports a relative path that climbs past the directory depth owning the file.
fn escapes(depth: Option<usize>, literal: &str) -> bool {
    let Some(mut depth) = depth else { return false };
    for part in literal.split('/') {
        match part {
            ".." => {
                if depth == 0 {
                    return true;
                }
                depth -= 1;
            }
            "" | "." => {}
            _ => depth += 1,
        }
    }
    false
}
