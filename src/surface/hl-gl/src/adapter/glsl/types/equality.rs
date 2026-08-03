use std::collections::{BTreeMap, BTreeSet};

use super::{Lexical, Token, Type, Types};

struct Equality<'a> {
    source: &'a str,
    types: Types,
    tokens: Vec<Token>,
    helpers: BTreeMap<String, String>,
}

impl<'a> Equality<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            types: Types::parse(source),
            tokens: Lexical::tokenize(source),
            helpers: BTreeMap::new(),
        }
    }

    fn lower(mut self) -> Option<String> {
        let comparisons = self.comparisons();
        if comparisons.is_empty() {
            return None;
        }

        let mut required = BTreeSet::new();
        for comparison in &comparisons {
            self.require_structs(&comparison.ty, &mut required)?;
        }
        for name in &required {
            let mut candidate = format!("hl_struct_equal_{name}");
            let mut serial = 0usize;
            while self.tokens.iter().any(|token| token.text == candidate) {
                serial += 1;
                candidate = format!("hl_struct_equal_{name}_{serial}");
            }
            self.helpers.insert(name.clone(), candidate);
        }

        let mut output = self.source.to_owned();
        for comparison in comparisons.into_iter().rev() {
            let helper = &self.helpers[&comparison.ty.name];
            let call = format!(
                "{}{}({}, {})",
                if comparison.not_equal { "!" } else { "" },
                helper,
                comparison.left,
                comparison.right
            );
            output.replace_range(comparison.start..comparison.end, &call);
        }

        let mut emitted = BTreeSet::new();
        let mut declarations = String::new();
        for name in required {
            self.emit_helper(&name, &mut emitted, &mut declarations)?;
        }
        let main = output.find("void main")?;
        output.insert_str(main, &declarations);
        Some(output)
    }

    fn comparisons(&self) -> Vec<Comparison> {
        let mut out = Vec::new();
        let mut at = 0usize;
        while at + 1 < self.tokens.len() {
            let not_equal = self.tokens[at].text == "!" && self.tokens[at + 1].text == "=";
            let equal = self.tokens[at].text == "=" && self.tokens[at + 1].text == "=";
            if !equal && !not_equal {
                at += 1;
                continue;
            }
            let Some(left_start) = self.operand_start(at) else {
                at += 2;
                continue;
            };
            let Some(right_end) = self.operand_end(at + 2) else {
                at += 2;
                continue;
            };
            let left = &self.source[self.tokens[left_start].start..self.tokens[at].start];
            let right = &self.source[self.tokens[at + 2].start..self.tokens[right_end - 1].end];
            let Some(left_ty) = self.types.expression(self.tokens[at].start, left.trim()) else {
                at += 2;
                continue;
            };
            let Some(right_ty) = self.types.expression(self.tokens[at].start, right.trim()) else {
                at += 2;
                continue;
            };
            if left_ty != right_ty
                || left_ty.is_array()
                || self.types.structure(left_ty.name()).is_none()
            {
                at += 2;
                continue;
            }
            out.push(Comparison {
                start: self.tokens[left_start].start,
                end: self.tokens[right_end - 1].end,
                left: left.trim().to_owned(),
                right: right.trim().to_owned(),
                ty: left_ty,
                not_equal,
            });
            at = right_end;
        }
        out
    }

    fn operand_start(&self, end: usize) -> Option<usize> {
        let last = end.checked_sub(1)?;
        let mut start = match self.tokens[last].text.as_str() {
            ")" => {
                let open = self.matching_open(last, "(", ")")?;
                if open > 0 && self.tokens[open - 1].identifier() {
                    open - 1
                } else {
                    open
                }
            }
            "]" => {
                let open = self.matching_open(last, "[", "]")?;
                self.operand_start(open)?
            }
            _ if self.tokens[last].identifier() => last,
            _ => return None,
        };
        while start > 0 && self.tokens[start - 1].text == "." {
            start = self.operand_start(start - 1)?;
        }
        Some(start)
    }

    fn operand_end(&self, start: usize) -> Option<usize> {
        let first = self.tokens.get(start)?;
        let mut end = if first.text == "(" {
            Lexical::matching(&self.tokens, start, "(", ")")? + 1
        } else if first.identifier() {
            if self
                .tokens
                .get(start + 1)
                .is_some_and(|token| token.text == "(")
            {
                Lexical::matching(&self.tokens, start + 1, "(", ")")? + 1
            } else {
                start + 1
            }
        } else {
            return None;
        };
        loop {
            if self.tokens.get(end).is_some_and(|token| token.text == ".")
                && self.tokens.get(end + 1).is_some_and(Token::identifier)
            {
                end += 2;
                continue;
            }
            if self.tokens.get(end).is_some_and(|token| token.text == "[") {
                end = Lexical::matching(&self.tokens, end, "[", "]")? + 1;
                continue;
            }
            return Some(end);
        }
    }

    fn matching_open(&self, close: usize, left: &str, right: &str) -> Option<usize> {
        let mut depth = 0usize;
        for at in (0..=close).rev() {
            if self.tokens[at].text == right {
                depth += 1;
            } else if self.tokens[at].text == left {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at);
                }
            }
        }
        None
    }

    fn require_structs(&self, ty: &Type, required: &mut BTreeSet<String>) -> Option<()> {
        if ty.is_array() && ty.array_dimensions().iter().any(Option::is_none) {
            return None;
        }
        let Some(structure) = self.types.structure(ty.name()) else {
            return Some(());
        };
        if !required.insert(ty.name().to_owned()) {
            return Some(());
        }
        for (_, field) in structure.fields() {
            self.require_structs(field, required)?;
        }
        Some(())
    }

    fn emit_helper(
        &self,
        name: &str,
        emitted: &mut BTreeSet<String>,
        output: &mut String,
    ) -> Option<()> {
        if !emitted.insert(name.to_owned()) {
            return Some(());
        }
        let structure = self.types.structure(name)?;
        for (_, field) in structure.fields() {
            if self.types.structure(field.name()).is_some() {
                self.emit_helper(field.name(), emitted, output)?;
            }
        }
        let comparisons = structure
            .fields()
            .map(|(field, ty)| {
                self.compare(ty, &format!("left.{field}"), &format!("right.{field}"))
            })
            .collect::<Option<Vec<_>>>()?;
        output.push_str(&format!(
            "bool {}({name} left, {name} right) {{ return {}; }}\n",
            self.helpers[name],
            if comparisons.is_empty() {
                "true".to_owned()
            } else {
                comparisons.join(" && ")
            }
        ));
        Some(())
    }

    fn compare(&self, ty: &Type, left: &str, right: &str) -> Option<String> {
        if let Some(extent) = ty.array_dimensions().first() {
            let extent = (*extent)?;
            let element = ty.clone().indexed()?;
            return (0..extent)
                .map(|index| {
                    self.compare(
                        &element,
                        &format!("{left}[{index}]"),
                        &format!("{right}[{index}]"),
                    )
                })
                .collect::<Option<Vec<_>>>()
                .map(|parts| format!("({})", parts.join(" && ")));
        }
        if let Some(helper) = self.helpers.get(ty.name()) {
            return Some(format!("{helper}({left}, {right})"));
        }
        if matches!(
            ty.name(),
            "vec2"
                | "vec3"
                | "vec4"
                | "ivec2"
                | "ivec3"
                | "ivec4"
                | "uvec2"
                | "uvec3"
                | "uvec4"
                | "bvec2"
                | "bvec3"
                | "bvec4"
        ) {
            return Some(format!("all(equal({left}, {right}))"));
        }
        if ty.name().starts_with("mat") {
            let columns = ty.name().as_bytes().get(3)?.checked_sub(b'0')? as usize;
            return Some(format!(
                "({})",
                (0..columns)
                    .map(|column| format!("all(equal({left}[{column}], {right}[{column}]))"))
                    .collect::<Vec<_>>()
                    .join(" && ")
            ));
        }
        Some(format!("({left} == {right})"))
    }
}

struct Comparison {
    start: usize,
    end: usize,
    left: String,
    right: String,
    ty: Type,
    not_equal: bool,
}

pub(crate) struct StructEquality;

impl StructEquality {
    pub(crate) fn lower(source: &mut String) {
        if let Some(lowered) = Equality::new(source).lower() {
            *source = lowered;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StructEquality;

    fn lower(source: &str) -> String {
        let mut source = source.to_owned();
        StructEquality::lower(&mut source);
        source
    }

    #[test]
    fn basic_and_nested_structs_use_typed_recursive_helpers() {
        let source = "struct T { vec3 vector; int scalar; }; struct S { float value; T nested; }; void main(){ S a; S b; bool same = a == b; bool different = a != b; }";
        let lowered = lower(source);
        assert!(
            lowered.contains("bool hl_struct_equal_T(T left, T right)"),
            "{lowered}"
        );
        assert!(
            lowered.contains("all(equal(left.vector, right.vector))"),
            "{lowered}"
        );
        assert!(
            lowered.contains("bool hl_struct_equal_S(S left, S right)"),
            "{lowered}"
        );
        assert!(
            lowered.contains("hl_struct_equal_T(left.nested, right.nested)"),
            "{lowered}"
        );
        assert!(lowered.contains("hl_struct_equal_S(a, b)"), "{lowered}");
        assert!(lowered.contains("!hl_struct_equal_S(a, b)"), "{lowered}");
        assert!(!lowered.contains("a == b"), "{lowered}");
        assert!(!lowered.contains("a != b"), "{lowered}");
    }

    #[test]
    fn fixed_arrays_and_matrices_expand_every_element_and_column() {
        let source = "struct S { vec2 samples[2]; mat3 basis; }; void main(){ S a; S b; bool same = a == b; }";
        let lowered = lower(source);
        for comparison in [
            "all(equal(left.samples[0], right.samples[0]))",
            "all(equal(left.samples[1], right.samples[1]))",
            "all(equal(left.basis[0], right.basis[0]))",
            "all(equal(left.basis[1], right.basis[1]))",
            "all(equal(left.basis[2], right.basis[2]))",
        ] {
            assert!(
                lowered.contains(comparison),
                "missing {comparison}: {lowered}"
            );
        }
    }

    #[test]
    fn parenthesized_indexed_function_operands_are_each_written_once() {
        let source = "struct S { int value; }; S values[2]; S make(int value){ return S(value); } int next(); void main(){ bool same = ((make(next()))) == values[next()]; }";
        let lowered = lower(source);
        assert!(
            lowered.contains("hl_struct_equal_S(((make(next()))), values[next()])"),
            "{lowered}"
        );
        assert_eq!(
            lowered.matches("make(next())").count(),
            1,
            "left operand duplicated: {lowered}"
        );
        assert_eq!(
            lowered.matches("values[next()]").count(),
            1,
            "right operand duplicated: {lowered}"
        );
    }

    #[test]
    fn lexical_shadowing_prevents_scalar_comparisons_from_being_rewritten() {
        let source = "struct S { int value; }; S left; S right; void main(){ { int left; int right; bool scalar = left == right; } bool aggregate = left == right; }";
        let lowered = lower(source);
        assert!(lowered.contains("bool scalar = left == right"), "{lowered}");
        assert!(
            lowered.contains("bool aggregate = hl_struct_equal_S(left, right)"),
            "{lowered}"
        );
    }

    #[test]
    fn scalar_vector_and_mismatched_controls_are_byte_exact() {
        for source in [
            "void main(){ int a; int b; bool value = a == b; }",
            "void main(){ vec3 a; vec3 b; bvec3 value = a == b; }",
            "void main(){ mat3 a; mat3 b; bool value = a == b; }",
            "struct S { int value; }; void main(){ S a; int b; bool value = a == b; }",
        ] {
            assert_eq!(lower(source), source);
        }
    }
}
