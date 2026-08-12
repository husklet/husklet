use std::{collections::BTreeSet, fs, path::Path};

use tree_sitter::Node;

use super::{parse, source_files, suppression};
use crate::{Finding, LintError, Location, Result, Severity, rule::Rule, source::Workspace};

const RULE: &str = "c-source-policy";

/// One caller-selected prohibition on named C function calls.
#[derive(Clone, Debug)]
pub struct CallPolicy {
    id: &'static str,
    names: BTreeSet<String>,
    message: String,
    help: String,
}

impl CallPolicy {
    /// Creates a prohibition with a stable rule identifier and actionable text.
    pub fn new(
        id: &'static str,
        names: impl IntoIterator<Item = impl Into<String>>,
        message: impl Into<String>,
        help: impl Into<String>,
    ) -> Self {
        Self {
            id,
            names: names.into_iter().map(Into::into).collect(),
            message: message.into(),
            help: help.into(),
        }
    }
}

/// Repository-independent C source policy assembled by its caller.
///
/// The engine has no pathname exceptions and prohibits no API by default.
/// Projects opt into policies explicitly, so ordinary C facilities such as
/// `getenv` remain valid unless a caller deliberately says otherwise.
#[derive(Default)]
pub struct Policy {
    calls: Vec<CallPolicy>,
}

impl Policy {
    /// Creates an empty policy.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one function-call policy.
    #[must_use]
    pub fn forbid_calls(mut self, policy: CallPolicy) -> Self {
        self.calls.push(policy);
        self
    }

    fn known_rules(&self) -> BTreeSet<&'static str> {
        self.calls.iter().map(|policy| policy.id).collect()
    }
}

impl Rule for Policy {
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
            findings.extend(analyze(&path, &source, self)?);
        }
        Ok(findings)
    }
}

fn analyze(path: &Path, source: &str, policy: &Policy) -> Result<Vec<Finding>> {
    let tree = parse(path, source)?;
    let mut candidates = Vec::new();
    collect_calls(tree.root_node(), source.as_bytes(), path, policy, &mut candidates);
    let owned = policy.known_rules();
    let mut known = owned.clone();
    known.extend(["c-file-length", "c-function-length", "c-maximum-nesting"]);
    Ok(suppression::apply(
        path,
        source,
        tree.root_node(),
        &known,
        &owned,
        true,
        candidates,
    ))
}

fn collect_calls(node: Node<'_>, source: &[u8], path: &Path, policy: &Policy, output: &mut Vec<Finding>) {
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
        && function.kind() == "identifier"
        && let Ok(name) = function.utf8_text(source)
    {
        for rule in policy.calls.iter().filter(|rule| rule.names.contains(name)) {
            let point = function.start_position();
            let mut finding = Finding::error(
                rule.id,
                name,
                Location {
                    path: path.to_owned(),
                    line: point.row + 1,
                    column: point.column + 1,
                    source: name.to_owned(),
                },
            );
            finding.message.clone_from(&rule.message);
            finding.help.clone_from(&rule.help);
            output.push(finding);
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_calls(child, source, path, policy, output);
    }
}

#[cfg(test)]
#[path = "policy_test.rs"]
mod test;
