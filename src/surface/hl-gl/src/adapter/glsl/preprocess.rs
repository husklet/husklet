//! The GLSL ES 1.00 preprocessor (GLSL ES 1.00 §3.4), owned by the guest driver.
//!
//! The driver is the GLES2 implementation: it reflects each stage's declarations and REGENERATES them as
//! desktop GLSL, so it must see the source AFTER macro expansion and conditional selection. A `#define`d
//! precision qualifier (`HIGHP_OR_DEFAULT`) or array size (`[MAX_POINT_LIGHTS]`) is otherwise reflected as a
//! type name or a non-constant dimension, and any surviving macro identifier is rejected by the host
//! compiler as an unknown variable. Preprocessing therefore happens HERE, exactly once, and not in the host
//! translator.
//!
//! Implemented: `#define` (object- and function-like, including an empty replacement list), `#undef`,
//! `#if`/`#ifdef`/`#ifndef`/`#elif`/`#else`/`#endif` with the `defined` operator and integral constant
//! expression evaluation ([`condition`]), `#error`, and the predefined `__LINE__`, `__FILE__`, `__VERSION__`,
//! `GL_ES` and `GL_FRAGMENT_PRECISION_HIGH` macros.
//!
//! Passed through unchanged: `#version`, `#extension`, `#pragma`, `#line`. These carry meaning for the stage
//! that consumes the preprocessed text — the host translator still keys its ES normalisation on
//! `#version … es` — so consuming them here would change the forwarded dialect.
//!
//! Rejected with a diagnostic (never silently forwarded): any other directive, `#`/`##` in a replacement
//! list (GLSL ES defines neither operator), a function-like invocation whose argument list does not close on
//! the same logical line, an argument-count mismatch, recursive expansion, a non-constant `#if` expression,
//! and unbalanced conditionals.

use super::Source;

mod comment;
mod condition;
mod define;
mod error;
mod word;

pub use error::PreprocessError;

pub(super) use condition::{Expression, Unknown, Values};
pub(super) use word::Words;

use define::Macros;

/// Nesting cap for macro expansion; exceeding it reports [`PreprocessError::MacroDepth`] instead of
/// recursing until the stack faults.
const MAX_EXPANSION_DEPTH: usize = 32;

impl Source<'_> {
    /// Comment removal (GLSL ES 1.00 §3.3) followed by preprocessing (§3.4).
    ///
    /// # Errors
    /// [`PreprocessError`] when the stage uses a construct this preprocessor rejects. The caller must fail
    /// the compile/link — forwarding partially preprocessed source produces an unknown-variable error from
    /// the host compiler with no attribution.
    pub fn preprocessed(self) -> Result<String, PreprocessError> {
        Preprocessor::default().apply(&self.comments_removed())
    }
}

/// One conditional nesting level.
#[derive(Debug)]
struct Branch {
    /// Whether the enclosing region is being emitted at all.
    enclosing: bool,
    /// Whether THIS arm is being emitted.
    live: bool,
    /// Whether any arm of this `#if` chain has been taken.
    resolved: bool,
    /// Whether `#else` has been seen, after which `#elif` is invalid.
    closed: bool,
}

#[derive(Debug, Default)]
struct Preprocessor {
    macros: Macros,
    branches: Vec<Branch>,
    out: String,
}

impl Preprocessor {
    /// Line numbering is preserved: every consumed directive and every skipped line emits a bare newline, so
    /// a host-reported line number still refers to the application's source line.
    fn apply(mut self, source: &str) -> Result<String, PreprocessError> {
        let mut line = 0usize;
        for logical in LogicalLines::from(source) {
            line += 1;
            let first = line;
            line += logical.continued;
            let trimmed = logical.text.trim_start();
            if let Some(rest) = trimmed.strip_prefix('#') {
                self.directive(rest, first)?;
            } else if self.live() {
                let expanded = self.macros.expand(&logical.text, first)?;
                self.out.push_str(&expanded);
            }
            for _ in 0..=logical.continued {
                self.out.push('\n');
            }
        }
        match self.branches.last() {
            Some(_) => Err(PreprocessError::UnterminatedConditional { line }),
            None => Ok(self.out),
        }
    }

    fn live(&self) -> bool {
        self.branches.last().is_none_or(|branch| branch.live)
    }

    fn directive(&mut self, rest: &str, line: usize) -> Result<(), PreprocessError> {
        let rest = rest.trim_start();
        let name_end = rest
            .find(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
            .unwrap_or(rest.len());
        let (name, tail) = rest.split_at(name_end);
        match name {
            // Conditional structure is tracked even inside a dead region, so nesting stays balanced.
            "if" | "ifdef" | "ifndef" => {
                let enclosing = self.live();
                let live = enclosing && self.condition(name, tail, line)?;
                self.branches.push(Branch {
                    enclosing,
                    live,
                    resolved: live,
                    closed: false,
                });
                Ok(())
            }
            "elif" | "else" => {
                let Some(branch) = self.branches.pop() else {
                    return Err(PreprocessError::ConditionalNesting {
                        line,
                        directive: name.to_owned(),
                    });
                };
                if branch.closed {
                    return Err(PreprocessError::ConditionalNesting {
                        line,
                        directive: name.to_owned(),
                    });
                }
                let take = branch.enclosing
                    && !branch.resolved
                    && (name == "else" || self.condition("if", tail, line)?);
                self.branches.push(Branch {
                    enclosing: branch.enclosing,
                    live: take,
                    resolved: branch.resolved || take,
                    closed: name == "else",
                });
                Ok(())
            }
            "endif" => self
                .branches
                .pop()
                .map(|_| ())
                .ok_or(PreprocessError::ConditionalNesting {
                    line,
                    directive: name.to_owned(),
                }),
            _ if !self.live() => Ok(()),
            "define" => self.macros.define(tail, line),
            "undef" => {
                let target = tail.trim();
                if target.is_empty() || Words(target).whole().is_none() {
                    return Err(PreprocessError::MacroName {
                        line,
                        name: target.to_owned(),
                    });
                }
                self.macros.remove(target);
                Ok(())
            }
            "error" => Err(PreprocessError::Error {
                line,
                message: tail.trim().to_owned(),
            }),
            // Directives whose meaning belongs to the stage consumer, not to this pass.
            "version" | "extension" | "pragma" | "line" => {
                self.out.push('#');
                self.out.push_str(rest);
                Ok(())
            }
            // GLSL ES 1.00 §3.4 permits a null directive.
            "" if tail.trim().is_empty() => Ok(()),
            _ => Err(PreprocessError::UnknownDirective {
                line,
                name: name.to_owned(),
            }),
        }
    }

    /// Evaluate a `#if`/`#elif` controlling expression, or a `#ifdef`/`#ifndef` operand.
    fn condition(&self, kind: &str, tail: &str, line: usize) -> Result<bool, PreprocessError> {
        if kind == "ifdef" || kind == "ifndef" {
            let target = tail.trim();
            if Words(target).whole().is_none() {
                return Err(PreprocessError::MacroName {
                    line,
                    name: target.to_owned(),
                });
            }
            return Ok(self.macros.defined(target) == (kind == "ifdef"));
        }
        let resolved = self.macros.resolve_defined(tail);
        let expanded = self.macros.expand(&resolved, line)?;
        Expression::evaluate(&expanded, Unknown::Zero, &())
            .map(|value| value != 0)
            .ok_or(PreprocessError::Condition {
                line,
                expression: tail.trim().to_owned(),
            })
    }
}

/// One logical source line: a physical line plus every line joined onto it by a trailing `\`
/// (GLSL ES 3.00 §3.1 line continuation). `continued` is how many extra physical lines it consumed.
struct SourceLine {
    text: String,
    continued: usize,
}

struct LogicalLines<'a> {
    lines: std::iter::Peekable<std::str::Lines<'a>>,
}

impl<'a> From<&'a str> for LogicalLines<'a> {
    fn from(source: &'a str) -> Self {
        Self {
            lines: source.lines().peekable(),
        }
    }
}

impl Iterator for LogicalLines<'_> {
    type Item = SourceLine;

    fn next(&mut self) -> Option<SourceLine> {
        let mut text = self.lines.next()?.to_owned();
        let mut continued = 0usize;
        while text.ends_with('\\') {
            text.pop();
            let Some(more) = self.lines.next() else {
                break;
            };
            text.push_str(more);
            continued += 1;
        }
        Some(SourceLine { text, continued })
    }
}
