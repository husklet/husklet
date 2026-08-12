use std::{collections::BTreeSet, fs, path::Path};

use tree_sitter::{Node, Parser};

use crate::{Finding, LintError, Location, Result, Severity, rule::Rule, source::Workspace};

const RULE: &str = "c-source-policy";
const SUPPRESSION_RULE: &str = "c-lint-suppression";

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
        for path in workspace.source_files()?.into_iter().filter(|path| is_c(path)) {
            let source = fs::read_to_string(&path).map_err(|error| LintError::io("read", &path, error))?;
            findings.extend(analyze(&path, &source, self)?);
        }
        Ok(findings)
    }
}

fn is_c(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "c" | "h" | "m" | "mm"))
}

#[derive(Debug)]
struct Suppression {
    line: usize,
    target: usize,
    rule: String,
    source: String,
    valid: bool,
}

fn analyze(path: &Path, source: &str, policy: &Policy) -> Result<Vec<Finding>> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .map_err(|error| parse_error(path, error.to_string()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| parse_error(path, "parser returned no syntax tree"))?;
    let lines = source.lines().collect::<Vec<_>>();
    let known = policy.known_rules();
    let (suppressions, mut findings) = suppressions(path, source, tree.root_node(), &lines, &known);
    let mut candidates = Vec::new();
    collect_calls(tree.root_node(), source.as_bytes(), path, policy, &mut candidates);

    for mut suppression in suppressions {
        if !suppression.valid {
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
            [] => findings.push(suppression_finding(
                path,
                suppression.line,
                &suppression.source,
                "stale or unnecessary suppression",
                "remove it or place it immediately before the violating source line",
            )),
            _ => findings.push(suppression_finding(
                path,
                suppression.line,
                &suppression.source,
                "overbroad suppression",
                "split the source expression so one annotation suppresses exactly one diagnostic",
            )),
        }
        suppression.valid = false;
    }
    findings.extend(candidates);
    findings.sort_by_key(|finding| {
        (
            finding.location.path.clone(),
            finding.location.line,
            finding.location.column,
        )
    });
    Ok(findings)
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

fn suppressions(
    path: &Path,
    source: &str,
    root: Node<'_>,
    lines: &[&str],
    known: &BTreeSet<&str>,
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
            findings.push(suppression_finding(
                path,
                line,
                raw,
                "malformed suppression",
                "use `// hl-lint: allow(rule-id) -- reason`",
            ));
            continue;
        };
        let Some((rule, reason)) = rest.split_once(") -- ") else {
            findings.push(suppression_finding(
                path,
                line,
                raw,
                "malformed suppression",
                "include one rule identifier and a non-empty reason",
            ));
            continue;
        };
        let reason = reason.trim().trim_end_matches("*/").trim();
        let syntactically_valid = !rule.is_empty()
            && rule
                .chars()
                .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-')
            && !reason.is_empty();
        if !syntactically_valid {
            findings.push(suppression_finding(
                path,
                line,
                raw,
                "malformed suppression",
                "use a lowercase kebab-case rule identifier and non-empty reason",
            ));
            continue;
        }
        if !known.contains(rule) {
            findings.push(suppression_finding(
                path,
                line,
                raw,
                "unknown suppression rule",
                "name a rule enabled by the caller's C policy",
            ));
            continue;
        }
        let target = lines
            .iter()
            .enumerate()
            .skip(index + 1)
            .find(|(_, candidate)| {
                let candidate = candidate.trim();
                !candidate.is_empty()
                    && !candidate.starts_with("//")
                    && !candidate.starts_with("/*")
                    && !candidate.starts_with('*')
            })
            .map_or(line + 1, |(target, _)| target + 1);
        parsed.push(Suppression {
            line,
            target,
            rule: rule.to_owned(),
            source: (*raw).to_owned(),
            valid: true,
        });
    }
    (parsed, findings)
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

fn suppression_finding(path: &Path, line: usize, source: &str, message: &str, help: &str) -> Finding {
    let column = source.find("hl-lint:").map_or(1, |column| column + 1);
    let mut finding = Finding::error(
        SUPPRESSION_RULE,
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

fn parse_error(path: &Path, message: impl Into<String>) -> LintError {
    LintError::io(
        "parse",
        path,
        std::io::Error::new(std::io::ErrorKind::InvalidData, message.into()),
    )
}

#[cfg(test)]
#[path = "policy_test.rs"]
mod test;
