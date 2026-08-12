use std::{collections::BTreeSet, fs, path::Path};

use tree_sitter::{Node, Parser};

use super::{source_files, suppression};
use crate::{Finding, LintError, Location, Result, Review, Severity, rule::Rule, source::Workspace};

const FILE_LINES: usize = 1_500;
const FUNCTION_LINES: usize = 200;
const NESTING: usize = 6;

/// Reports oversized C files, functions, and control-flow nesting from an embedded C syntax tree.
pub struct Structure;

impl Rule for Structure {
    fn id(&self) -> &'static str {
        "c-source-structure"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for path in source_files(workspace)? {
            let text = fs::read_to_string(&path).map_err(|error| LintError::io("read", &path, error))?;
            findings.extend(analyze(&path, &text)?);
        }
        Ok(findings)
    }
}

fn analyze(path: &Path, text: &str) -> Result<Vec<Finding>> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .map_err(|error| parse_error(path, error.to_string()))?;
    let tree = parser
        .parse(text, None)
        .ok_or_else(|| parse_error(path, "parser returned no syntax tree"))?;
    let clean = without_comments(text, tree.root_node());
    let effective = effective_lines(&clean, 0, clean.len());
    let mut findings = Vec::new();
    if effective > FILE_LINES {
        findings.push(metric(path, 1, 1, "file length", effective, FILE_LINES));
    }
    visit_functions(tree.root_node(), &clean, path, &mut findings);
    let rules = BTreeSet::from(["c-file-length", "c-function-length", "c-maximum-nesting"]);
    Ok(suppression::apply(
        path,
        text,
        tree.root_node(),
        &rules,
        &rules,
        false,
        findings,
    ))
}

fn visit_functions(node: Node<'_>, clean: &str, path: &Path, findings: &mut Vec<Finding>) {
    if node.kind() == "function_definition" {
        let lines = effective_lines(clean, node.start_byte(), node.end_byte());
        let point = node.start_position();
        if lines > FUNCTION_LINES {
            findings.push(metric(
                path,
                point.row + 1,
                point.column + 1,
                "function length",
                lines,
                FUNCTION_LINES,
            ));
        }
        let nesting = maximum_nesting(node, 0);
        if nesting > NESTING {
            findings.push(metric(
                path,
                point.row + 1,
                point.column + 1,
                "function nesting",
                nesting,
                NESTING,
            ));
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_functions(child, clean, path, findings);
    }
}

fn maximum_nesting(node: Node<'_>, depth: usize) -> usize {
    let depth = depth
        + usize::from(matches!(
            node.kind(),
            "if_statement" | "switch_statement" | "for_statement" | "while_statement" | "do_statement"
        ));
    let mut maximum = depth;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        maximum = maximum.max(maximum_nesting(child, depth));
    }
    maximum
}

fn without_comments(text: &str, root: Node<'_>) -> String {
    let mut bytes = text.as_bytes().to_vec();
    erase_comments(root, &mut bytes);
    String::from_utf8(bytes).expect("comment replacement preserves UTF-8")
}

fn erase_comments(node: Node<'_>, bytes: &mut [u8]) {
    if node.kind() == "comment" {
        for byte in &mut bytes[node.byte_range()] {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = b' ';
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        erase_comments(child, bytes);
    }
}

fn effective_lines(text: &str, start: usize, end: usize) -> usize {
    text.get(start..end)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

fn metric(path: &Path, line: usize, column: usize, subject: &str, value: usize, limit: usize) -> Finding {
    let rule = match subject {
        "file length" => "c-file-length",
        "function length" => "c-function-length",
        "function nesting" => "c-maximum-nesting",
        _ => unreachable!("closed C structure metric"),
    };
    let mut finding = Finding::error(
        rule,
        subject,
        Location {
            path: path.to_owned(),
            line,
            column,
            source: String::new(),
        },
    );
    finding.message = format!("C {subject} is {value}; the review threshold is {limit}");
    finding.help = "split cohesive state and behavior behind a named C module boundary".into();
    let mut review = Review::error();
    review.metadata = vec![("value".into(), value.to_string()), ("limit".into(), limit.to_string())];
    review.questions = vec!["Which independent responsibility can move behind a narrow header?".into()];
    finding.review = Some(review);
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
#[path = "structure_test.rs"]
mod test;
