use std::{collections::BTreeSet, path::Path};

use tree_sitter::Node;

use crate::{Finding, Location};

const RULE: &str = "c-lint-suppression";

struct Suppression {
    line: usize,
    target: usize,
    rule: String,
    source: String,
}

pub(super) fn apply(
    path: &Path,
    source: &str,
    root: Node<'_>,
    known: &BTreeSet<&str>,
    owned: &BTreeSet<&str>,
    validate_unknown: bool,
    mut candidates: Vec<Finding>,
) -> Vec<Finding> {
    let lines = source.lines().collect::<Vec<_>>();
    let (suppressions, mut findings) = parse(path, source, root, &lines, known, validate_unknown);
    for suppression in suppressions {
        if !owned.contains(suppression.rule.as_str()) {
            continue;
        }
        let matches = candidates
            .iter()
            .enumerate()
            .filter(|(_, finding)| finding.rule == suppression.rule && finding.location.line == suppression.target)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [index] => {
                candidates.remove(*index);
            }
            [] => findings.push(finding(
                path,
                suppression.line,
                &suppression.source,
                "stale or unnecessary suppression",
                "remove it or place it immediately before the violating source line",
            )),
            _ => findings.push(finding(
                path,
                suppression.line,
                &suppression.source,
                "overbroad suppression",
                "split the source expression so one annotation suppresses exactly one diagnostic",
            )),
        }
    }
    findings.extend(candidates);
    findings.sort_by_key(|finding| (finding.location.line, finding.location.column));
    findings
}

fn parse(
    path: &Path,
    source: &str,
    root: Node<'_>,
    lines: &[&str],
    known: &BTreeSet<&str>,
    validate_unknown: bool,
) -> (Vec<Suppression>, Vec<Finding>) {
    let mut parsed = Vec::new();
    let mut findings = Vec::new();
    let mut comments = Vec::new();
    collect_comments(root, &mut comments);
    for comment in comments {
        let index = comment.start_position().row;
        let raw = lines.get(index).copied().unwrap_or_default();
        let text = comment.utf8_text(source.as_bytes()).unwrap_or_default();
        let Some(directive) = text.strip_prefix("//").map(str::trim) else {
            continue;
        };
        let Some(directive) = directive.strip_prefix("hl-lint:").map(str::trim) else {
            continue;
        };
        let line = index + 1;
        let Some(rest) = directive.strip_prefix("allow(") else {
            findings.push(finding(
                path,
                line,
                raw,
                "malformed suppression",
                "use `// hl-lint: allow(rule-id) -- reason`",
            ));
            continue;
        };
        let Some((rule, reason)) = rest.split_once(") -- ") else {
            findings.push(finding(
                path,
                line,
                raw,
                "malformed suppression",
                "include one rule identifier and a non-empty reason",
            ));
            continue;
        };
        let reason = reason.trim();
        if rule.is_empty()
            || !rule
                .chars()
                .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-')
            || reason.is_empty()
        {
            findings.push(finding(
                path,
                line,
                raw,
                "malformed suppression",
                "use a lowercase kebab-case rule identifier and non-empty reason",
            ));
            continue;
        }
        if !known.contains(rule) {
            if !validate_unknown {
                continue;
            }
            findings.push(finding(
                path,
                line,
                raw,
                "unknown suppression rule",
                "name a diagnostic enabled by this C rule",
            ));
            continue;
        }
        let target = lines
            .iter()
            .enumerate()
            .skip(index + 1)
            .find(|(_, candidate)| code_bearing(candidate))
            .map(|(target, _)| target + 1);
        let Some(target) = target else {
            findings.push(finding(
                path,
                line,
                raw,
                "stale suppression",
                "remove the annotation or restore its immediately following code",
            ));
            continue;
        };
        parsed.push(Suppression {
            line,
            target,
            rule: rule.to_owned(),
            source: raw.to_owned(),
        });
    }
    (parsed, findings)
}

fn code_bearing(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty() && !line.starts_with("//") && !line.starts_with("/*") && !line.starts_with('*')
}

fn collect_comments<'tree>(node: Node<'tree>, output: &mut Vec<Node<'tree>>) {
    if node.kind() == "comment" {
        output.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_comments(child, output);
    }
}

fn finding(path: &Path, line: usize, source: &str, message: &str, help: &str) -> Finding {
    let column = source.find("hl-lint:").map_or(1, |column| column + 1);
    let mut finding = Finding::error(
        RULE,
        message,
        Location {
            path: path.to_owned(),
            line,
            column,
            source: source.to_owned(),
        },
    );
    finding.message = message.to_owned();
    finding.help = help.to_owned();
    finding
}
