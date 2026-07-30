//! `#define` storage and macro expansion (GLSL ES 1.00 §3.4).

use super::{PreprocessError, Words, MAX_EXPANSION_DEPTH};
use std::collections::BTreeMap;

/// One `#define`d macro. GLSL ES has exactly these two forms; there is no `#`/`##` operator, so a
/// replacement list is stored as plain text and rescanned after substitution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Macro {
    Object(String),
    Function { params: Vec<String>, body: String },
}

/// The macro table, including the predefined macros GLSL ES 1.00 §3.4 requires. `__LINE__` is resolved per
/// line by the caller, so it is not stored here.
#[derive(Debug)]
pub(super) struct Macros {
    entries: BTreeMap<String, Macro>,
}

/// GLSL ES 1.00 §3.4 predefined macros. `GL_ES` is `1` for an ES shading-language implementation, which this
/// driver is; `GL_FRAGMENT_PRECISION_HIGH` is `1` because the host executes every stage on Metal, where the
/// `highp` range/precision requirements always hold.
const PREDEFINED: [(&str, &str); 4] = [
    ("__VERSION__", "100"),
    ("__FILE__", "0"),
    ("GL_ES", "1"),
    ("GL_FRAGMENT_PRECISION_HIGH", "1"),
];

impl Default for Macros {
    fn default() -> Self {
        Self {
            entries: PREDEFINED
                .iter()
                .map(|(name, body)| ((*name).to_owned(), Macro::Object((*body).to_owned())))
                .collect(),
        }
    }
}

impl Macros {
    pub(super) fn defined(&self, name: &str) -> bool {
        name == "__LINE__" || self.entries.contains_key(name)
    }

    pub(super) fn remove(&mut self, name: &str) {
        self.entries.remove(name);
    }

    /// Record a `#define`. `rest` is everything after the directive name.
    pub(super) fn define(&mut self, rest: &str, line: usize) -> Result<(), PreprocessError> {
        let rest = rest.trim_start();
        let name_end = rest
            .find(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
            .unwrap_or(rest.len());
        let name = &rest[..name_end];
        if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) || name == "defined" {
            return Err(PreprocessError::MacroName {
                line,
                name: name.to_owned(),
            });
        }
        let tail = &rest[name_end..];
        // A `(` IMMEDIATELY after the name (no space) makes the macro function-like.
        let entry = if let Some(parameters) = tail.strip_prefix('(') {
            let Some(close) = parameters.find(')') else {
                return Err(PreprocessError::MacroParameters {
                    line,
                    name: name.to_owned(),
                });
            };
            let list = parameters[..close].trim();
            let params = if list.is_empty() {
                Vec::new()
            } else {
                list.split(',')
                    .map(|param| param.trim().to_owned())
                    .collect::<Vec<_>>()
            };
            if params.iter().any(|param| {
                param.is_empty()
                    || param.starts_with(|c: char| c.is_ascii_digit())
                    || !param.bytes().all(Words::is_continuation)
            }) {
                return Err(PreprocessError::MacroParameters {
                    line,
                    name: name.to_owned(),
                });
            }
            Macro::Function {
                params,
                body: parameters[close + 1..].trim().to_owned(),
            }
        } else {
            Macro::Object(tail.trim().to_owned())
        };
        let body = match &entry {
            Macro::Object(body) | Macro::Function { body, .. } => body.as_str(),
        };
        if body.contains('#') {
            return Err(PreprocessError::TokenPaste {
                line,
                name: name.to_owned(),
            });
        }
        self.entries.insert(name.to_owned(), entry);
        Ok(())
    }

    /// Replace `defined NAME` / `defined(NAME)` with `1`/`0`. Must run BEFORE expansion so the operand is not
    /// itself macro-expanded (GLSL ES 1.00 §3.4).
    pub(super) fn resolve_defined(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let bytes = text.as_bytes();
        let mut at = 0usize;
        while at < bytes.len() {
            let Some(word) = Words(text).at(at) else {
                at = Words(text).copy(at, &mut out);
                continue;
            };
            if word != "defined" {
                out.push_str(word);
                at += word.len();
                continue;
            }
            let mut cursor = at + word.len();
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            let parenthesized = bytes.get(cursor) == Some(&b'(');
            if parenthesized {
                cursor += 1;
                while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                    cursor += 1;
                }
            }
            let Some(operand) = Words(text).at(cursor) else {
                out.push_str(word);
                at += word.len();
                continue;
            };
            cursor += operand.len();
            if parenthesized {
                while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                    cursor += 1;
                }
                if bytes.get(cursor) != Some(&b')') {
                    out.push_str(word);
                    at += word.len();
                    continue;
                }
                cursor += 1;
            }
            out.push(if self.defined(operand) { '1' } else { '0' });
            at = cursor;
        }
        out
    }

    /// Fully expand `text`, substituting `__LINE__` with `line`.
    pub(super) fn expand(&self, text: &str, line: usize) -> Result<String, PreprocessError> {
        let mut out = String::with_capacity(text.len());
        self.substitute(text, line, &mut Vec::new(), 0, &mut out)?;
        Ok(out)
    }

    fn substitute(
        &self,
        text: &str,
        line: usize,
        blocked: &mut Vec<String>,
        depth: usize,
        out: &mut String,
    ) -> Result<(), PreprocessError> {
        let bytes = text.as_bytes();
        let mut at = 0usize;
        while at < bytes.len() {
            let Some(word) = Words(text).at(at) else {
                at = Words(text).copy(at, out);
                continue;
            };
            at += word.len();
            if word == "__LINE__" {
                out.push_str(&line.to_string());
                continue;
            }
            let entry = self.entries.get(word);
            if blocked.iter().any(|open| open == word) || entry.is_none() {
                out.push_str(word);
                continue;
            }
            if depth >= MAX_EXPANSION_DEPTH {
                return Err(PreprocessError::MacroDepth {
                    line,
                    name: word.to_owned(),
                });
            }
            let replacement = match entry {
                None => {
                    out.push_str(word);
                    continue;
                }
                Some(Macro::Object(body)) => body.clone(),
                Some(Macro::Function { params, body }) => {
                    let (arguments, end) = self.arguments(text, at, line, word)?;
                    if arguments.len() != params.len() {
                        return Err(PreprocessError::MacroArguments {
                            line,
                            name: word.to_owned(),
                            expected: params.len(),
                            found: arguments.len(),
                        });
                    }
                    at = end;
                    let mut expanded = Vec::with_capacity(arguments.len());
                    for argument in &arguments {
                        let mut value = String::new();
                        self.substitute(argument, line, blocked, depth + 1, &mut value)?;
                        expanded.push(value);
                    }
                    replace_params(body, params, &expanded)
                }
            };
            blocked.push(word.to_owned());
            let result = self.substitute(&replacement, line, blocked, depth + 1, out);
            blocked.pop();
            result?;
        }
        Ok(())
    }

    /// Parse a function-like macro's `( a, b )` argument list starting at `at`, returning the arguments and
    /// the offset just past the closing paren. Commas inside nested parens/brackets do not separate.
    fn arguments(
        &self,
        text: &str,
        at: usize,
        line: usize,
        name: &str,
    ) -> Result<(Vec<String>, usize), PreprocessError> {
        let invocation = || PreprocessError::MacroInvocation {
            line,
            name: name.to_owned(),
        };
        let bytes = text.as_bytes();
        let mut cursor = at;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'(') {
            return Err(invocation());
        }
        cursor += 1;
        let mut arguments = Vec::new();
        let mut start = cursor;
        let mut depth = 0usize;
        loop {
            match bytes.get(cursor) {
                None => return Err(invocation()),
                Some(b'(' | b'[') => depth += 1,
                Some(b')') if depth == 0 => {
                    let argument = text[start..cursor].trim();
                    if !argument.is_empty() || !arguments.is_empty() {
                        arguments.push(argument.to_owned());
                    }
                    return Ok((arguments, cursor + 1));
                }
                Some(b')' | b']') => depth = depth.saturating_sub(1),
                Some(b',') if depth == 0 => {
                    arguments.push(text[start..cursor].trim().to_owned());
                    start = cursor + 1;
                }
                Some(_) => {}
            }
            cursor += 1;
        }
    }
}

/// Substitute each parameter name in `body` with its expanded argument.
fn replace_params(body: &str, params: &[String], arguments: &[String]) -> String {
    let mut out = String::with_capacity(body.len());
    let bytes = body.as_bytes();
    let mut at = 0usize;
    while at < bytes.len() {
        let Some(word) = Words(body).at(at) else {
            at = Words(body).copy(at, &mut out);
            continue;
        };
        at += word.len();
        match params.iter().position(|param| param == word) {
            Some(index) => out.push_str(arguments.get(index).map_or("", String::as_str)),
            None => out.push_str(word),
        }
    }
    out
}
