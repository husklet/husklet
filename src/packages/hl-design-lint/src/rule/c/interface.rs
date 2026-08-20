use std::{collections::BTreeSet, fs, path::Path};

use tree_sitter::Node;

use super::{parse, source_files, suppression};
use crate::{
    Budget, CInterfacePolicy, Finding, LintError, Location, Result, Review, Severity, rule::Rule, source::Workspace,
};

const RULE: &str = "c-interface-breadth";

/// Reviews C headers that expose more operations than one cohesive interface should own.
pub struct Interface {
    policy: CInterfacePolicy,
}

impl Interface {
    /// Creates the rule with a repository-owned, portable threshold.
    #[must_use]
    pub const fn new(policy: CInterfacePolicy) -> Self {
        Self { policy }
    }
}

impl Rule for Interface {
    fn id(&self) -> &'static str {
        RULE
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for path in source_files(workspace)? {
            if path.extension().and_then(|extension| extension.to_str()) != Some("h") {
                continue;
            }
            let source = fs::read_to_string(&path).map_err(|error| LintError::io("read", &path, error))?;
            findings.extend(analyze(&path, &source, self.policy.maximum_functions)?);
        }
        Ok(findings)
    }
}

fn analyze(path: &Path, source: &str, maximum: usize) -> Result<Vec<Finding>> {
    let tree = parse(path, source)?;
    let declarations = declarations(tree.root_node(), source);
    let candidates = declarations
        .first()
        .filter(|_| declarations.len() > maximum)
        .map_or_else(Vec::new, |first| {
            vec![finding(path, *first, declarations.len(), maximum)]
        });
    let rules = BTreeSet::from([RULE]);
    Ok(suppression::apply(
        path,
        source,
        tree.root_node(),
        &rules,
        &rules,
        false,
        candidates,
    ))
}

fn declarations(root: Node<'_>, source: &str) -> Vec<usize> {
    let mut lines = Vec::new();
    let mut cursor = root.walk();
    for node in root.named_children(&mut cursor) {
        let text = node.utf8_text(source.as_bytes()).unwrap_or_default().trim_start();
        if node.kind() == "declaration" && contains(node, "function_declarator") && !text.starts_with("static ") {
            lines.push(node.start_position().row + 1);
        }
    }
    lines
}

fn contains(node: Node<'_>, kind: &str) -> bool {
    if node.kind() == kind {
        return true;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor).any(|child| contains(child, kind))
}

fn finding(path: &Path, line: usize, count: usize, maximum: usize) -> Finding {
    let mut finding = Finding::warning(
        RULE,
        "header interface",
        Location {
            path: path.to_owned(),
            line,
            column: 1,
            source: String::new(),
        },
    );
    finding.message = format!("C header exposes {count} function declarations; the configured maximum is {maximum}");
    finding.help = "split unrelated operations into cohesive headers with narrow ownership".into();
    finding.budget = Some(Budget {
        unit: "declaration",
        measured: count,
        limit: maximum,
    });
    let mut review = Review::error();
    review.metadata = vec![
        ("functions".into(), count.to_string()),
        ("maximum".into(), maximum.to_string()),
    ];
    review.questions = vec!["Which declarations share one lifecycle, state owner, and change reason?".into()];
    finding.review = Some(review);
    finding
}

#[cfg(test)]
#[path = "interface_test.rs"]
mod tests;
