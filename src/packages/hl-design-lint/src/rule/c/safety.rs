use std::{collections::BTreeSet, fs, path::Path};

use tree_sitter::Node;

use super::{parse, source_files, suppression};
use crate::{CSafetyPolicy, Finding, LintError, Location, Result, Severity, rule::Rule, source::Workspace};

const RULE: &str = "c-safety-rationale";

/// Requires attached safety rationale for configured C operations with caller-owned invariants.
pub struct Safety {
    operations: BTreeSet<String>,
}

impl Safety {
    /// Creates the rule from exact, repository-owned operation names.
    #[must_use]
    pub fn new(policy: CSafetyPolicy) -> Self {
        Self {
            operations: policy.operations.into_iter().collect(),
        }
    }
}

impl Rule for Safety {
    fn id(&self) -> &'static str {
        RULE
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for path in source_files(workspace)? {
            let source = fs::read_to_string(&path).map_err(|error| LintError::io("read", &path, error))?;
            findings.extend(analyze(&path, &source, &self.operations)?);
        }
        Ok(findings)
    }
}

fn analyze(path: &Path, source: &str, operations: &BTreeSet<String>) -> Result<Vec<Finding>> {
    let tree = parse(path, source)?;
    let lines = source.lines().collect::<Vec<_>>();
    let mut findings = Vec::new();
    collect(tree.root_node(), source, &lines, operations, path, &mut findings);
    let rules = BTreeSet::from([RULE]);
    Ok(suppression::apply(
        path,
        source,
        tree.root_node(),
        &rules,
        &rules,
        false,
        findings,
    ))
}

fn collect(
    node: Node<'_>,
    source: &str,
    lines: &[&str],
    operations: &BTreeSet<String>,
    path: &Path,
    output: &mut Vec<Finding>,
) {
    if node.kind() == "call_expression"
        && let Some(function) = node
            .child_by_field_name("function")
            .filter(|child| child.kind() == "identifier")
        && let Ok(name) = function.utf8_text(source.as_bytes())
        && operations.contains(name)
        && !has_rationale(lines, node.start_position().row)
    {
        output.push(finding(path, node, name));
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect(child, source, lines, operations, path, output);
    }
}

fn has_rationale(lines: &[&str], call_row: usize) -> bool {
    let mut row = call_row;
    while row > 0 {
        row -= 1;
        let line = lines.get(row).copied().unwrap_or_default().trim();
        if !is_comment(line) {
            return false;
        }
        if line
            .split_once("SAFETY:")
            .is_some_and(|(_, reason)| !reason.trim().is_empty())
        {
            return true;
        }
    }
    false
}

fn is_comment(line: &str) -> bool {
    line.starts_with("//") || line.starts_with("/*") || line.starts_with('*') || line.ends_with("*/")
}

fn finding(path: &Path, node: Node<'_>, name: &str) -> Finding {
    let point = node.start_position();
    let mut finding = Finding::error(
        RULE,
        name,
        Location {
            path: path.to_owned(),
            line: point.row + 1,
            column: point.column + 1,
            source: String::new(),
        },
    );
    finding.message = format!("configured safety-sensitive C operation `{name}` has no attached `SAFETY:` rationale");
    finding.help =
        "state the pointer, lifetime, bounds, ownership, or concurrency invariant immediately above the call".into();
    finding
}

#[cfg(test)]
#[path = "safety_test.rs"]
mod tests;
