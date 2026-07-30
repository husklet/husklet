//! Integral constant expression evaluation.
//!
//! Two GLSL ES rules need the same evaluator: the controlling expression of `#if`/`#elif` (GLSL ES 1.00
//! §3.4, which admits the full integer operator set including `defined`) and an array size, which must be an
//! integral constant expression (GLSL ES 1.00 §4.1.9). The two differ only in what an unresolved identifier
//! means, which [`Unknown`] selects.

/// How an identifier with no known value is treated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Unknown {
    /// `#if` semantics: an undefined macro evaluates to `0` (C99 6.10.1, and what every GLSL
    /// implementation real shaders are written against does).
    Zero,
    /// Array-size semantics: an unresolved name is not a constant, so the expression is rejected.
    Reject,
}

/// Named integer constants an expression may reference: `#define`d object-like macros already substituted by
/// the caller, plus the stage's global `const int`/`const uint` declarations.
pub(crate) trait Values {
    fn value(&self, name: &str) -> Option<i64>;
}

impl Values for () {
    fn value(&self, _: &str) -> Option<i64> {
        None
    }
}

impl Values for std::collections::BTreeMap<String, i64> {
    fn value(&self, name: &str) -> Option<i64> {
        self.get(name).copied()
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(i64),
    Name(String),
    Operator(&'static str),
}

/// Longest-first so `<<` never lexes as two `<`.
const OPERATORS: [&str; 24] = [
    "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+", "-", "*", "/", "%", "<", ">", "&", "|",
    "^", "~", "!", "(", ")", "?", ":",
];

const MAX_DEPTH: usize = 32;

impl Token {
    /// Tokenize a controlling expression. `None` when a character cannot begin any token.
    fn lex(text: &str) -> Option<Vec<Self>> {
        let bytes = text.as_bytes();
        let mut tokens = Vec::new();
        let mut at = 0usize;
        while at < bytes.len() {
            let byte = bytes[at];
            if byte.is_ascii_whitespace() {
                at += 1;
                continue;
            }
            if byte.is_ascii_digit() {
                let start = at;
                while at < bytes.len() && bytes[at].is_ascii_alphanumeric() {
                    at += 1;
                }
                tokens.push(Self::Number(Self::integer(&text[start..at])?));
                continue;
            }
            if byte == b'_' || byte.is_ascii_alphabetic() {
                let start = at;
                while at < bytes.len() && (bytes[at] == b'_' || bytes[at].is_ascii_alphanumeric()) {
                    at += 1;
                }
                tokens.push(Self::Name(text[start..at].to_owned()));
                continue;
            }
            let operator = OPERATORS
                .iter()
                .find(|operator| text[at..].starts_with(**operator))?;
            tokens.push(Self::Operator(operator));
            at += operator.len();
        }
        Some(tokens)
    }

    /// A GLSL ES integer literal: decimal, `0x` hex, or leading-zero octal, with an optional `u`/`U` suffix.
    fn integer(literal: &str) -> Option<i64> {
        let digits = literal.trim_end_matches(['u', 'U']);
        let parsed = if let Some(hex) = digits
            .strip_prefix("0x")
            .or_else(|| digits.strip_prefix("0X"))
        {
            i64::from_str_radix(hex, 16)
        } else if digits.len() > 1 && digits.starts_with('0') {
            i64::from_str_radix(&digits[1..], 8)
        } else {
            digits.parse::<i64>()
        };
        parsed.ok()
    }
}

/// Recursive-descent evaluation of the lexed expression. Depth is capped so hostile input cannot overflow
/// the stack.
pub(crate) struct Expression<'a, V: Values> {
    tokens: Vec<Token>,
    at: usize,
    unknown: Unknown,
    values: &'a V,
}

impl<'a, V: Values> Expression<'a, V> {
    /// `None` when `text` is not an integral constant expression under `unknown`.
    pub(crate) fn evaluate(text: &str, unknown: Unknown, values: &'a V) -> Option<i64> {
        let tokens = Token::lex(text)?;
        if tokens.is_empty() {
            return None;
        }
        let mut expression = Self {
            tokens,
            at: 0,
            unknown,
            values,
        };
        let value = expression.ternary(0)?;
        (expression.at == expression.tokens.len()).then_some(value)
    }

    fn operator(&self, symbol: &str) -> bool {
        matches!(self.tokens.get(self.at), Some(Token::Operator(found)) if *found == symbol)
    }

    fn take(&mut self, symbol: &str) -> bool {
        if self.operator(symbol) {
            self.at += 1;
            return true;
        }
        false
    }

    fn ternary(&mut self, depth: usize) -> Option<i64> {
        if depth > MAX_DEPTH {
            return None;
        }
        let condition = self.binary(0, depth + 1)?;
        if !self.take("?") {
            return Some(condition);
        }
        let taken = self.ternary(depth + 1)?;
        if !self.take(":") {
            return None;
        }
        let other = self.ternary(depth + 1)?;
        Some(if condition != 0 { taken } else { other })
    }

    /// Precedence climbing over the binary operators, lowest level first.
    fn binary(&mut self, level: usize, depth: usize) -> Option<i64> {
        const LEVELS: [&[&str]; 10] = [
            &["||"],
            &["&&"],
            &["|"],
            &["^"],
            &["&"],
            &["==", "!="],
            &["<=", ">=", "<", ">"],
            &["<<", ">>"],
            &["+", "-"],
            &["*", "/", "%"],
        ];
        if depth > MAX_DEPTH {
            return None;
        }
        let Some(operators) = LEVELS.get(level) else {
            return self.unary(depth + 1);
        };
        let mut left = self.binary(level + 1, depth + 1)?;
        loop {
            let Some(symbol) = operators
                .iter()
                .find(|symbol| self.operator(symbol))
                .copied()
            else {
                return Some(left);
            };
            self.at += 1;
            let right = self.binary(level + 1, depth + 1)?;
            left = apply(symbol, left, right)?;
        }
    }

    fn unary(&mut self, depth: usize) -> Option<i64> {
        if depth > MAX_DEPTH {
            return None;
        }
        if self.take("+") {
            return self.unary(depth + 1);
        }
        if self.take("-") {
            return self.unary(depth + 1)?.checked_neg();
        }
        if self.take("~") {
            return Some(!self.unary(depth + 1)?);
        }
        if self.take("!") {
            return Some(i64::from(self.unary(depth + 1)? == 0));
        }
        if self.take("(") {
            let value = self.ternary(depth + 1)?;
            return self.take(")").then_some(value);
        }
        match self.tokens.get(self.at)?.clone() {
            Token::Number(value) => {
                self.at += 1;
                Some(value)
            }
            Token::Name(name) => {
                self.at += 1;
                match (self.values.value(&name), self.unknown) {
                    (Some(value), _) => Some(value),
                    (None, Unknown::Zero) => Some(0),
                    (None, Unknown::Reject) => None,
                }
            }
            Token::Operator(_) => None,
        }
    }
}

fn apply(symbol: &str, left: i64, right: i64) -> Option<i64> {
    Some(match symbol {
        "||" => i64::from(left != 0 || right != 0),
        "&&" => i64::from(left != 0 && right != 0),
        "|" => left | right,
        "^" => left ^ right,
        "&" => left & right,
        "==" => i64::from(left == right),
        "!=" => i64::from(left != right),
        "<=" => i64::from(left <= right),
        ">=" => i64::from(left >= right),
        "<" => i64::from(left < right),
        ">" => i64::from(left > right),
        "<<" => left.checked_shl(u32::try_from(right).ok()?)?,
        ">>" => left.checked_shr(u32::try_from(right).ok()?)?,
        "+" => left.checked_add(right)?,
        "-" => left.checked_sub(right)?,
        "*" => left.checked_mul(right)?,
        "/" => left.checked_div(right)?,
        "%" => left.checked_rem(right)?,
        _ => return None,
    })
}
