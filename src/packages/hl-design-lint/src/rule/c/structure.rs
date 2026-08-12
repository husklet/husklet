use std::{fs, path::Path};

use super::source_files;
use crate::{Finding, LintError, Location, Result, Review, Severity, rule::Rule, source::Workspace};

const FILE_LINES: usize = 1_500;
const FUNCTION_LINES: usize = 200;
const NESTING: usize = 6;

/// Reports oversized C files and functions with language-aware lexical structure.
pub struct Structure;

impl Rule for Structure {
    fn id(&self) -> &'static str {
        "c-source-structure"
    }

    fn severity(&self) -> Severity {
        // The retained engine predates these cohesion thresholds. Keep the migration visible
        // without making historic size debt block correctness and analyzer gates.
        Severity::Warning
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for path in source_files(workspace)? {
            let text = fs::read_to_string(&path).map_err(|error| LintError::io("read", &path, error))?;
            findings.extend(analyze(&path, &text));
        }
        Ok(findings)
    }
}

#[derive(Default)]
struct Lexical {
    block_comment: bool,
    quote: Option<u8>,
}

fn analyze(path: &Path, text: &str) -> Vec<Finding> {
    let mut lexical = Lexical::default();
    let clean = text.lines().map(|line| lexical.clean(line)).collect::<Vec<_>>();
    let effective = clean.iter().filter(|line| !line.trim().is_empty()).count();
    let mut findings = Vec::new();
    if effective > FILE_LINES {
        findings.push(metric(path, 1, "file length", effective, FILE_LINES));
    }
    for function in functions(&clean) {
        if function.lines > FUNCTION_LINES {
            findings.push(metric(
                path,
                function.start,
                "function length",
                function.lines,
                FUNCTION_LINES,
            ));
        }
        if function.nesting > NESTING {
            findings.push(metric(
                path,
                function.start,
                "function nesting",
                function.nesting,
                NESTING,
            ));
        }
    }
    findings
}

struct Function {
    start: usize,
    lines: usize,
    nesting: usize,
}

fn functions(lines: &[String]) -> Vec<Function> {
    let mut output = Vec::new();
    let mut signature = String::new();
    let mut signature_start = 0;
    let mut function: Option<Function> = None;
    let mut depth = 0usize;
    for (index, line) in lines.iter().enumerate() {
        let number = index + 1;
        let trimmed = line.trim();
        if function.is_none() && depth == 0 {
            if signature.is_empty() && candidate_start(trimmed) {
                signature_start = number;
                signature.push_str(trimmed);
            } else if !signature.is_empty() {
                signature.push(' ');
                signature.push_str(trimmed);
            }
            if !signature.is_empty() && trimmed.contains(';') {
                signature.clear();
            } else if !signature.is_empty() && trimmed.contains('{') {
                if signature_like_function(&signature) {
                    function = Some(Function {
                        start: signature_start,
                        lines: 0,
                        nesting: 1,
                    });
                }
                signature.clear();
            }
        }
        if let Some(current) = function.as_mut()
            && !trimmed.is_empty()
        {
            current.lines += 1;
        }
        for byte in line.bytes() {
            match byte {
                b'{' => {
                    depth += 1;
                    if let Some(current) = function.as_mut() {
                        current.nesting = current.nesting.max(depth);
                    }
                }
                b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0
                        && let Some(current) = function.take()
                    {
                        output.push(current);
                    }
                }
                _ => {}
            }
        }
    }
    output
}

fn candidate_start(line: &str) -> bool {
    !line.starts_with('#')
        && line.contains('(')
        && !["if", "for", "while", "switch", "return", "sizeof", "_Static_assert"]
            .iter()
            .any(|keyword| line.starts_with(keyword))
}

fn signature_like_function(signature: &str) -> bool {
    signature.contains('(')
        && signature.contains(')')
        && !signature.contains(';')
        && !signature.trim_start().starts_with("typedef")
}

impl Lexical {
    fn clean(&mut self, line: &str) -> String {
        let bytes = line.as_bytes();
        let mut output = String::new();
        let mut index = 0;
        while index < bytes.len() {
            if self.block_comment {
                if bytes.get(index..index + 2) == Some(b"*/") {
                    self.block_comment = false;
                    index += 2;
                } else {
                    index += 1;
                }
            } else if self.quote.is_none() && bytes.get(index..index + 2) == Some(b"//") {
                break;
            } else if self.quote.is_none() && bytes.get(index..index + 2) == Some(b"/*") {
                self.block_comment = true;
                index += 2;
            } else if let Some(quote) = self.quote {
                if bytes[index] == b'\\' {
                    index += 2;
                } else {
                    if bytes[index] == quote {
                        self.quote = None;
                    }
                    index += 1;
                }
            } else if matches!(bytes[index], b'\'' | b'"') {
                self.quote = Some(bytes[index]);
                index += 1;
            } else {
                output.push(bytes[index] as char);
                index += 1;
            }
        }
        output
    }
}

fn metric(path: &Path, line: usize, subject: &str, value: usize, limit: usize) -> Finding {
    let mut finding = Finding::warning(
        "c-source-structure",
        subject,
        Location {
            path: path.to_owned(),
            line,
            column: 1,
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

#[cfg(test)]
#[path = "structure_test.rs"]
mod test;
