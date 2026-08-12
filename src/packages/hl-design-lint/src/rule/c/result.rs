use std::{collections::BTreeSet, fs, path::Path};

use tree_sitter::{Node, Parser};

use super::{source_files, suppression};
use crate::{CResultPolicy, Finding, LintError, Location, Result, Severity, rule::Rule, source::Workspace};

const RULE: &str = "c-ignored-result";

/// Reports configured C calls whose return value is discarded as an expression statement.
pub struct ResultUse {
    functions: BTreeSet<String>,
}

impl ResultUse {
    /// Creates the rule from exact, repository-owned function names.
    #[must_use]
    pub fn new(policy: CResultPolicy) -> Self {
        Self {
            functions: policy.must_use_functions.into_iter().collect(),
        }
    }
}

impl Rule for ResultUse {
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
            findings.extend(analyze(&path, &source, &self.functions)?);
        }
        Ok(findings)
    }
}

fn analyze(path: &Path, source: &str, functions: &BTreeSet<String>) -> Result<Vec<Finding>> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .map_err(|error| parse_error(path, error.to_string()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| parse_error(path, "parser returned no syntax tree"))?;
    let mut findings = Vec::new();
    collect(tree.root_node(), source, functions, path, &mut findings);
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

fn collect(node: Node<'_>, source: &str, functions: &BTreeSet<String>, path: &Path, output: &mut Vec<Finding>) {
    if node.kind() == "expression_statement"
        && let Some(call) = node.named_child(0).filter(|child| child.kind() == "call_expression")
        && let Some(function) = call
            .child_by_field_name("function")
            .filter(|child| child.kind() == "identifier")
        && let Ok(name) = function.utf8_text(source.as_bytes())
        && functions.contains(name)
    {
        output.push(finding(path, node, name));
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect(child, source, functions, path, output);
    }
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
    finding.message = format!("result of configured must-use C function `{name}` is discarded");
    finding.help =
        "check and handle the result, return it to the caller, or explicitly document a narrow suppression".into();
    finding
}

fn parse_error(path: &Path, message: impl Into<String>) -> LintError {
    LintError::io(
        "parse",
        path,
        std::io::Error::new(std::io::ErrorKind::InvalidData, message.into()),
    )
}

#[cfg(test)]
#[path = "result_test.rs"]
mod tests;
