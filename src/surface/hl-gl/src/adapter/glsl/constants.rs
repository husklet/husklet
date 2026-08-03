//! Named integer constants usable as an array size.
//!
//! GLSL ES 1.00 §4.1.9 requires an array size to be an integral constant expression, and §4.3.2 makes a
//! global `const int` initialised by such an expression exactly that. After preprocessing a `#define`d size
//! is already a literal, so the remaining named sizes are the stage's `const int`/`const uint` globals.

use super::preprocess::{Expression, Values, Words};
use super::Source;
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub(super) struct Constants {
    values: BTreeMap<String, i64>,
}

impl Values for Constants {
    fn value(&self, name: &str) -> Option<i64> {
        self.values.get(name).copied()
    }
}

impl Constants {
    /// Collect the integral `const` globals of one preprocessed, comment-free stage. Declarations are folded
    /// in source order, so a later constant may be defined in terms of an earlier one.
    pub(super) fn from_source(source: &str) -> Self {
        let mut constants = Self::default();
        for declaration in Source::new(source).consts() {
            constants.insert(&declaration);
        }
        constants
    }

    /// The array size `dimension` denotes, or `None` when it is not an integral constant expression.
    pub(super) fn dimension(&self, dimension: &str) -> Option<u32> {
        let value = Expression::evaluate(dimension, self)?;
        u32::try_from(value).ok().filter(|size| *size > 0)
    }

    /// `const [precision] int NAME = <integral constant expression>;`. Anything else — a float/vector
    /// constant, an array, or several declarators — cannot size an array and is skipped.
    fn insert(&mut self, declaration: &str) {
        let Some(body) = declaration.trim().strip_prefix("const") else {
            return;
        };
        let Some(body) = body.trim_start().strip_suffix(';') else {
            return;
        };
        let Some((head, expression)) = body.split_once('=') else {
            return;
        };
        let mut words = head
            .split_whitespace()
            .filter(|word| !matches!(*word, "lowp" | "mediump" | "highp"));
        if !matches!(words.next(), Some("int" | "uint")) {
            return;
        }
        let Some(name) = words.next().and_then(|word| Words(word).whole()) else {
            return;
        };
        if words.next().is_some() {
            return;
        }
        if let Some(value) = Expression::evaluate(expression.trim(), self) {
            self.values.insert(name.to_owned(), value);
        }
    }
}
