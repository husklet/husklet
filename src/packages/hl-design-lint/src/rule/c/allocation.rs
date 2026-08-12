use std::{collections::BTreeSet, fs, path::Path};

use tree_sitter::{Node, Parser};

use super::{source_files, suppression};
use crate::{CAllocationPolicy, Finding, LintError, Location, Result, Severity, rule::Rule, source::Workspace};

const RULE: &str = "c-unchecked-allocation";

/// Reports configured nullable allocations dereferenced without a syntactic null check.
pub struct Allocation(BTreeSet<String>);

impl Allocation {
    /// Creates the rule from exact allocator names.
    #[must_use]
    pub fn new(policy: CAllocationPolicy) -> Self {
        Self(policy.functions.into_iter().collect())
    }
}

impl Rule for Allocation {
    fn id(&self) -> &'static str {
        RULE
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut output = Vec::new();
        for path in source_files(workspace)? {
            let source = fs::read_to_string(&path).map_err(|error| LintError::io("read", &path, error))?;
            output.extend(analyze(&path, &source, &self.0)?);
        }
        Ok(output)
    }
}

fn analyze(path: &Path, source: &str, allocators: &BTreeSet<String>) -> Result<Vec<Finding>> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .map_err(|error| parse_error(path, error.to_string()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| parse_error(path, "parser returned no syntax tree"))?;
    let mut findings = Vec::new();
    visit_functions(tree.root_node(), source, allocators, path, &mut findings);
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

fn visit_functions(
    node: Node<'_>,
    source: &str,
    allocators: &BTreeSet<String>,
    path: &Path,
    output: &mut Vec<Finding>,
) {
    if node.kind() == "function_definition" {
        let mut declarations = Vec::new();
        allocations(node, source, allocators, &mut declarations);
        let function = node.utf8_text(source.as_bytes()).unwrap_or_default();
        for (name, declaration) in declarations {
            if dereferenced(node, source, &name) && !null_checked(function, &name) {
                output.push(finding(path, declaration, &name));
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit_functions(child, source, allocators, path, output);
    }
}

fn allocations<'tree>(
    node: Node<'tree>,
    source: &str,
    allocators: &BTreeSet<String>,
    output: &mut Vec<(String, Node<'tree>)>,
) {
    if node.kind() == "init_declarator"
        && let Some(value) = node.child_by_field_name("value")
        && value.kind() == "call_expression"
        && let Some(function) = value.child_by_field_name("function")
        && let Ok(function) = function.utf8_text(source.as_bytes())
        && allocators.contains(function)
        && let Some(declarator) = node.child_by_field_name("declarator")
        && let Some(name) = identifier(declarator, source)
    {
        output.push((name, node));
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        allocations(child, source, allocators, output);
    }
}

fn identifier(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() == "identifier" {
        return node.utf8_text(source.as_bytes()).ok().map(str::to_owned);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| identifier(child, source))
}

fn dereferenced(node: Node<'_>, source: &str, name: &str) -> bool {
    let text = node.utf8_text(source.as_bytes()).unwrap_or_default().replace(' ', "");
    if matches!(
        node.kind(),
        "subscript_expression" | "field_expression" | "pointer_expression"
    ) && (text.starts_with(&format!("{name}["))
        || text.starts_with(&format!("{name}->"))
        || text.starts_with(&format!("*{name}")))
    {
        return true;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|child| dereferenced(child, source, name))
}

fn null_checked(function: &str, name: &str) -> bool {
    let compact = function
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    [
        format!("{name}==NULL"),
        format!("{name}!=NULL"),
        format!("NULL=={name}"),
        format!("NULL!={name}"),
        format!("if({name})"),
        format!("if(!{name})"),
    ]
    .iter()
    .any(|pattern| compact.contains(pattern))
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
    finding.message =
        format!("nullable allocation assigned to `{name}` is dereferenced without a null check in the function");
    finding.help =
        "check the allocation result before dereference, or use an allocator with a documented non-null contract"
            .into();
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
#[path = "allocation_test.rs"]
mod tests;
