//! Lexical GLSL type information used by source-to-source lowerings.
//!
//! This is deliberately a small source model, not a second GLSL validator. The guest compiler has already
//! accepted the shader before translation reaches this module. Its job is to retain the type facts a
//! syntax-directed lowering needs while respecting GLSL's value shadowing and block lifetimes.

use std::collections::BTreeMap;

mod equality;
mod lexical;
mod sampler;
pub(super) use equality::StructEquality;
use lexical::TokenStream as Lexical;
pub(super) use sampler::StructSamplers;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Type {
    name: String,
    arrays: Vec<Option<usize>>,
}

impl Type {
    fn named(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            arrays: Vec::new(),
        }
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn is_array(&self) -> bool {
        !self.arrays.is_empty()
    }

    pub(super) fn array_dimensions(&self) -> &[Option<usize>] {
        &self.arrays
    }

    fn arrays(mut self, dimensions: Vec<Option<usize>>) -> Self {
        self.arrays.extend(dimensions);
        self
    }

    fn indexed(mut self) -> Option<Self> {
        if !self.arrays.is_empty() {
            self.arrays.remove(0);
            return Some(self);
        }
        match self.name.as_str() {
            "vec2" | "vec3" | "vec4" => Some(Self::named("float")),
            "ivec2" | "ivec3" | "ivec4" => Some(Self::named("int")),
            "uvec2" | "uvec3" | "uvec4" => Some(Self::named("uint")),
            "bvec2" | "bvec3" | "bvec4" => Some(Self::named("bool")),
            "mat2" => Some(Self::named("vec2")),
            "mat3" => Some(Self::named("vec3")),
            "mat4" => Some(Self::named("vec4")),
            "mat2x3" | "mat2x4" => Some(Self::named("vec2")),
            "mat3x2" | "mat3x4" => Some(Self::named("vec3")),
            "mat4x2" | "mat4x3" => Some(Self::named("vec4")),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Struct {
    fields: BTreeMap<String, Type>,
}

impl Struct {
    pub(super) fn field(&self, name: &str) -> Option<&Type> {
        self.fields.get(name)
    }

    pub(super) fn fields(&self) -> impl Iterator<Item = (&str, &Type)> {
        self.fields.iter().map(|(name, ty)| (name.as_str(), ty))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Function {
    result: Type,
    parameters: Vec<Type>,
}

impl Function {
    pub(super) fn result(&self) -> &Type {
        &self.result
    }

    pub(super) fn parameters(&self) -> &[Type] {
        &self.parameters
    }
}

#[derive(Clone, Debug)]
struct Token {
    text: String,
    start: usize,
    end: usize,
}

impl Token {
    fn identifier(&self) -> bool {
        self.text
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
    }

    fn qualifier(&self) -> bool {
        matches!(
            self.text.as_str(),
            "const"
                | "attribute"
                | "varying"
                | "uniform"
                | "in"
                | "out"
                | "inout"
                | "lowp"
                | "mediump"
                | "highp"
                | "centroid"
                | "flat"
                | "smooth"
        )
    }
}

#[derive(Clone, Debug)]
struct Scope {
    start: usize,
    end: usize,
    parent: Option<usize>,
}

#[derive(Clone, Debug)]
struct Variable {
    name: String,
    ty: Type,
    declared_at: usize,
    scope: usize,
}

/// Type facts indexed by their source lexical scope.
#[derive(Clone, Debug)]
pub(super) struct Types {
    structs: BTreeMap<String, Struct>,
    functions: BTreeMap<String, Vec<Function>>,
    scopes: Vec<Scope>,
    variables: Vec<Variable>,
}

impl Types {
    pub(super) fn parse(source: &str) -> Self {
        let tokens = Lexical::tokenize(source);
        let (scopes, token_scopes) = Lexical::scopes(&tokens, source.len());
        let mut types = Self {
            structs: BTreeMap::new(),
            functions: BTreeMap::new(),
            scopes,
            variables: Vec::new(),
        };
        let struct_ranges = types.collect_structs(&tokens);
        let function_ranges = types.collect_functions(&tokens, &token_scopes);
        types.collect_variables(&tokens, &token_scopes, &struct_ranges, &function_ranges);
        types
    }

    pub(super) fn structure(&self, name: &str) -> Option<&Struct> {
        self.structs.get(name)
    }

    pub(super) fn functions(&self, name: &str) -> &[Function] {
        self.functions.get(name).map_or(&[], Vec::as_slice)
    }

    /// Resolve an expression as it appears at `at` in the original source.
    pub(super) fn expression(&self, at: usize, expression: &str) -> Option<Type> {
        let tokens = Lexical::tokenize(expression);
        self.expression_tokens(at, &tokens)
    }

    fn expression_tokens(&self, at: usize, tokens: &[Token]) -> Option<Type> {
        let mut cursor = 0usize;
        let first = tokens.get(cursor)?;
        let mut ty = if first.text == "(" {
            let close = Lexical::matching(tokens, 0, "(", ")")?;
            let ty = self.expression_tokens(at, &tokens[1..close])?;
            cursor = close + 1;
            ty
        } else if tokens.get(1).is_some_and(|token| token.text == "(") {
            let close = Lexical::matching(&tokens, 1, "(", ")")?;
            cursor = close + 1;
            if self.known_type(&first.text) {
                Type::named(&first.text)
            } else {
                self.functions(&first.text).first()?.result.clone()
            }
        } else {
            cursor += 1;
            self.variable(at, &first.text)?
        };

        while cursor < tokens.len() {
            match tokens[cursor].text.as_str() {
                "." => {
                    let member = tokens.get(cursor + 1)?.text.as_str();
                    ty = self.member(&ty, member)?;
                    cursor += 2;
                }
                "[" => {
                    cursor = Lexical::matching(&tokens, cursor, "[", "]")? + 1;
                    ty = ty.indexed()?;
                }
                _ => return None,
            }
        }
        Some(ty)
    }

    fn known_type(&self, name: &str) -> bool {
        Type::builtin(name) || self.structs.contains_key(name)
    }

    fn variable(&self, at: usize, name: &str) -> Option<Type> {
        let mut scope = self
            .scopes
            .iter()
            .enumerate()
            .filter(|(_, scope)| scope.start <= at && at <= scope.end)
            .max_by_key(|(_, scope)| scope.start)
            .map(|(index, _)| index)?;
        loop {
            if let Some(variable) = self.variables.iter().rev().find(|variable| {
                variable.scope == scope && variable.name == name && variable.declared_at <= at
            }) {
                return Some(variable.ty.clone());
            }
            scope = self.scopes[scope].parent?;
        }
    }

    fn member(&self, ty: &Type, member: &str) -> Option<Type> {
        if ty.is_array() {
            return None;
        }
        if let Some(structure) = self.structure(ty.name()) {
            return structure.field(member).cloned();
        }
        let scalar = match ty.name() {
            "vec2" | "vec3" | "vec4" => "float",
            "ivec2" | "ivec3" | "ivec4" => "int",
            "uvec2" | "uvec3" | "uvec4" => "uint",
            "bvec2" | "bvec3" | "bvec4" => "bool",
            _ => return None,
        };
        let width_suffix = ty.name().as_bytes().last().copied()?;
        let component_set = |component| match width_suffix {
            b'2' => "xyrgst".contains(component),
            b'3' => "xyzrgbstp".contains(component),
            b'4' => "xyzwrgbastpq".contains(component),
            _ => false,
        };
        if !member.chars().all(component_set) {
            return None;
        }
        let width = member.chars().count();
        match width {
            1 => Some(Type::named(scalar)),
            2..=4 => Some(Type::named(match (scalar, width) {
                ("float", 2) => "vec2",
                ("float", 3) => "vec3",
                ("float", 4) => "vec4",
                ("int", 2) => "ivec2",
                ("int", 3) => "ivec3",
                ("int", 4) => "ivec4",
                ("uint", 2) => "uvec2",
                ("uint", 3) => "uvec3",
                ("uint", 4) => "uvec4",
                ("bool", 2) => "bvec2",
                ("bool", 3) => "bvec3",
                ("bool", 4) => "bvec4",
                _ => return None,
            })),
            _ => None,
        }
    }

    fn collect_structs(&mut self, tokens: &[Token]) -> Vec<(usize, usize)> {
        let mut ranges = Vec::new();
        for at in 0..tokens.len() {
            if tokens[at].text != "struct" || !tokens.get(at + 1).is_some_and(Token::identifier) {
                continue;
            }
            let Some(open) = tokens[at + 2..]
                .iter()
                .position(|token| token.text == "{")
                .map(|offset| at + 2 + offset)
            else {
                continue;
            };
            let Some(close) = Lexical::matching(tokens, open, "{", "}") else {
                continue;
            };
            let mut fields = BTreeMap::new();
            let mut cursor = open + 1;
            while cursor < close {
                let end = tokens[cursor..close]
                    .iter()
                    .position(|token| token.text == ";")
                    .map_or(close, |offset| cursor + offset);
                if let Some((ty, names)) = Lexical::declaration(tokens, cursor, end, |name| {
                    Type::builtin(name) || self.structs.contains_key(name)
                }) {
                    for (name, dimensions) in names {
                        fields.insert(name, ty.clone().arrays(dimensions));
                    }
                }
                cursor = end + 1;
            }
            self.structs
                .insert(tokens[at + 1].text.clone(), Struct { fields });
            ranges.push((at, close));
        }
        ranges
    }

    fn collect_functions(
        &mut self,
        tokens: &[Token],
        token_scopes: &[usize],
    ) -> Vec<(usize, usize)> {
        let mut ranges = Vec::new();
        for open in 0..tokens.len() {
            if tokens[open].text != "(" || open < 2 || !tokens[open - 1].identifier() {
                continue;
            }
            let result_at = Lexical::previous_type(tokens, open - 1, |name| self.known_type(name));
            let Some(result_at) = result_at else { continue };
            let Some(close) = Lexical::matching(tokens, open, "(", ")") else {
                continue;
            };
            if !tokens
                .get(close + 1)
                .is_some_and(|token| matches!(token.text.as_str(), "{" | ";"))
            {
                continue;
            }
            let parameters = Lexical::parameter_declarations(tokens, open + 1, close, |name| {
                self.known_type(name)
            });
            let signature = Function {
                result: Type::named(&tokens[result_at].text),
                parameters: parameters.iter().map(|(_, ty)| ty.clone()).collect(),
            };
            self.functions
                .entry(tokens[open - 1].text.clone())
                .or_default()
                .push(signature);
            if tokens[close + 1].text == "{" {
                let body_scope = token_scopes[close + 1];
                for (name, ty) in parameters {
                    self.variables.push(Variable {
                        name,
                        ty,
                        declared_at: tokens[close + 1].start,
                        scope: body_scope,
                    });
                }
                if let Some(body_close) = Lexical::matching(tokens, close + 1, "{", "}") {
                    ranges.push((result_at, body_close));
                }
            } else {
                ranges.push((result_at, close + 1));
            }
        }
        ranges
    }

    fn collect_variables(
        &mut self,
        tokens: &[Token],
        token_scopes: &[usize],
        struct_ranges: &[(usize, usize)],
        function_ranges: &[(usize, usize)],
    ) {
        let mut cursor = 0usize;
        while cursor < tokens.len() {
            if Lexical::inside(cursor, struct_ranges) {
                cursor += 1;
                continue;
            }
            let Some(type_at) = Lexical::next_type(tokens, cursor, |name| self.known_type(name))
            else {
                break;
            };
            if !Lexical::declaration_context(tokens, type_at)
                || Lexical::function_parameter(tokens, type_at)
            {
                cursor = type_at + 1;
                continue;
            }
            if Lexical::inside(type_at, function_ranges)
                && tokens
                    .get(type_at + 2)
                    .is_some_and(|token| token.text == "(")
            {
                cursor = type_at + 1;
                continue;
            }
            let end = Lexical::statement_end(tokens, type_at);
            if let Some((ty, names)) =
                Lexical::declaration(tokens, type_at, end, |name| self.known_type(name))
            {
                let scope = token_scopes[type_at];
                for (name, dimensions) in names {
                    self.variables.push(Variable {
                        name,
                        ty: ty.clone().arrays(dimensions),
                        declared_at: tokens[type_at].start,
                        scope,
                    });
                }
            }
            cursor = end.saturating_add(1).max(type_at + 1);
        }
    }
}

impl Type {
    fn builtin(name: &str) -> bool {
        matches!(
            name,
            "void"
                | "bool"
                | "int"
                | "uint"
                | "float"
                | "vec2"
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
                | "mat2"
                | "mat3"
                | "mat4"
                | "mat2x3"
                | "mat2x4"
                | "mat3x2"
                | "mat3x4"
                | "mat4x2"
                | "mat4x3"
                | "sampler2D"
                | "samplerCube"
                | "sampler3D"
                | "sampler2DArray"
                | "sampler2DShadow"
                | "samplerCubeShadow"
                | "sampler2DArrayShadow"
                | "isampler2D"
                | "isampler3D"
                | "isamplerCube"
                | "isampler2DArray"
                | "usampler2D"
                | "usampler3D"
                | "usamplerCube"
                | "usampler2DArray"
        )
    }
}

#[cfg(test)]
mod tests;
