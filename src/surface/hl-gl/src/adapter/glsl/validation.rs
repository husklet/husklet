use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScalarKind {
    Float,
    Int,
}

const TYPES: &[&str] = &[
    "void",
    "float",
    "int",
    "bool",
    "mat2",
    "mat3",
    "mat4",
    "vec2",
    "vec3",
    "vec4",
    "ivec2",
    "ivec3",
    "ivec4",
    "bvec2",
    "bvec3",
    "bvec4",
    "sampler2D",
    "samplerCube",
];

// GLSL ES 1.00 section 3.6.  The second set is reserved for possible future use and therefore has the
// same identifier restriction as the language keywords in an ES 1.00 shader.
const KEYWORDS: &[&str] = &[
    "attribute",
    "const",
    "uniform",
    "varying",
    "break",
    "continue",
    "do",
    "for",
    "while",
    "if",
    "else",
    "in",
    "out",
    "inout",
    "float",
    "int",
    "void",
    "bool",
    "true",
    "false",
    "lowp",
    "mediump",
    "highp",
    "precision",
    "invariant",
    "discard",
    "return",
    "mat2",
    "mat3",
    "mat4",
    "vec2",
    "vec3",
    "vec4",
    "ivec2",
    "ivec3",
    "ivec4",
    "bvec2",
    "bvec3",
    "bvec4",
    "sampler2D",
    "samplerCube",
    "struct",
];

const RESERVED_WORDS: &[&str] = &[
    "asm",
    "class",
    "union",
    "enum",
    "typedef",
    "template",
    "this",
    "packed",
    "goto",
    "switch",
    "default",
    "inline",
    "noinline",
    "volatile",
    "public",
    "static",
    "extern",
    "external",
    "interface",
    "flat",
    "long",
    "short",
    "double",
    "half",
    "fixed",
    "unsigned",
    "superp",
    "input",
    "output",
    "hvec2",
    "hvec3",
    "hvec4",
    "dvec2",
    "dvec3",
    "dvec4",
    "fvec2",
    "fvec3",
    "fvec4",
    "sampler1D",
    "sampler3D",
    "sampler1DShadow",
    "sampler2DShadow",
    "sampler2DRect",
    "sampler3DRect",
    "sampler2DRectShadow",
    "sizeof",
    "cast",
    "namespace",
    "using",
];

fn scalar_kind(token: &str) -> Option<ScalarKind> {
    match token {
        "float" | "vec2" | "vec3" | "vec4" => Some(ScalarKind::Float),
        "int" | "ivec2" | "ivec3" | "ivec4" => Some(ScalarKind::Int),
        _ => None,
    }
}

fn resolve<'a>(
    scopes: &[HashMap<&'a str, (&'a str, ScalarKind)>],
    name: &str,
) -> Option<(&'a str, ScalarKind)> {
    scopes
        .iter()
        .rev()
        .find_map(|scope| scope.get(name).copied())
}

fn tokens(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = source.as_bytes();
    let mut at = 0;
    let mut line_has_token = false;
    while at < bytes.len() {
        if bytes[at] == b'\n' {
            line_has_token = false;
            at += 1;
        } else if bytes[at] == b'#' && !line_has_token {
            // Preprocessor operators have their own GLSL ES 1.00 grammar. In particular `%` and the
            // bitwise operators are legal in `#if` expressions even though they are reserved in shader
            // expressions, so compile-time source validation must not tokenize directive bodies.
            loop {
                while at < bytes.len() && bytes[at] != b'\n' {
                    at += 1;
                }
                let continued = source[..at].trim_end().ends_with('\\');
                if at < bytes.len() {
                    at += 1;
                }
                if !continued {
                    break;
                }
            }
            line_has_token = false;
        } else if bytes[at..].starts_with(b"//") {
            at += 2;
            while at < bytes.len() && bytes[at] != b'\n' {
                at += 1;
            }
        } else if bytes[at..].starts_with(b"/*") {
            at += 2;
            while at + 1 < bytes.len() && !bytes[at..].starts_with(b"*/") {
                at += 1;
            }
            at = (at + 2).min(bytes.len());
        } else if bytes[at] == b'_' || bytes[at].is_ascii_alphanumeric() {
            line_has_token = true;
            let start = at;
            at += 1;
            while at < bytes.len() && (bytes[at] == b'_' || bytes[at].is_ascii_alphanumeric()) {
                at += 1;
            }
            out.push(source[start..at].to_string());
        } else {
            let remaining = &source[at..];
            if let Some(operator) = ["<<=", ">>=", "%=", "&=", "^=", "|="]
                .into_iter()
                .find(|operator| remaining.starts_with(operator))
            {
                line_has_token = true;
                out.push(operator.to_string());
                at += operator.len();
                continue;
            }
            if let Some(operator) = ["<<", ">>", "&&", "||", "<=", ">="]
                .into_iter()
                .find(|operator| remaining.starts_with(operator))
            {
                line_has_token = true;
                out.push(operator.to_string());
                at += operator.len();
                continue;
            }
            if matches!(
                bytes[at],
                b'+' | b'-'
                    | b'*'
                    | b'/'
                    | b'%'
                    | b'~'
                    | b'<'
                    | b'>'
                    | b'&'
                    | b'^'
                    | b'|'
                    | b';'
                    | b'{'
                    | b'}'
                    | b'='
                    | b','
                    | b'('
                    | b')'
                    | b'.'
            ) {
                line_has_token = true;
                out.push((bytes[at] as char).to_string());
            }
            at += 1;
        }
    }
    out
}

/// The first operator reserved by GLSL ES 1.00 that occurs as a source token.
///
/// Integer remainder and bitwise operators were deliberately reserved in ES 1.00 and became language
/// operators in ES 3.00.  Comments do not contribute tokens, and the legal ES 1.00 relational/logical
/// operators (`<`, `>`, `<=`, `>=`, `&&`, `||`) remain distinct tokens.
pub fn reserved_operator(source: &str) -> Option<String> {
    if super::declared_es_version(source) >= 300 {
        return None;
    }
    tokens(source).into_iter().find(|token| {
        matches!(
            token.as_str(),
            "%" | "~" | "<<" | ">>" | "&" | "^" | "|" | "%=" | "<<=" | ">>=" | "&=" | "^=" | "|="
        )
    })
}

fn identifier_is_reserved(identifier: &str) -> bool {
    KEYWORDS.contains(&identifier)
        || RESERVED_WORDS.contains(&identifier)
        || identifier.contains("__")
        || identifier.starts_with("gl_")
        || !identifier
            .as_bytes()
            .first()
            .is_some_and(|byte| *byte == b'_' || byte.is_ascii_alphabetic())
}

fn is_word_token(token: &str) -> bool {
    token
        .as_bytes()
        .first()
        .is_some_and(|byte| *byte == b'_' || byte.is_ascii_alphanumeric())
}

/// Return an ES 1.00 identifier used illegally as a declaration name.
///
/// This deliberately checks DECLARATOR position, not mere token presence: `if`, `return`, `float`, and
/// every other language keyword remain legal in their grammatical roles.  Names beginning `gl_`, names
/// containing two consecutive underscores, and digit-led tokens are reserved by section 3.6 too.
pub fn invalid_declaration_identifier(source: &str) -> Option<String> {
    let tokens = tokens(source);
    for declaration in tokens.windows(2) {
        if TYPES.contains(&declaration[0].as_str())
            && is_word_token(&declaration[1])
            && identifier_is_reserved(&declaration[1])
        {
            return Some(format!(
                "'{}' : reserved word may not be used as an identifier in GLSL ES 1.00",
                declaration[1]
            ));
        }
    }
    None
}

/// Diagnose storage/invariance qualifiers used outside their GLSL ES 1.00 declaration grammar.
pub fn invalid_storage_declaration(source: &str, shader_kind: u32) -> Option<String> {
    let tokens = tokens(source);
    let mut depth = 0usize;
    for (at, token) in tokens.iter().enumerate() {
        match token.as_str() {
            "{" => depth += 1,
            "}" => depth = depth.saturating_sub(1),
            "attribute" if depth != 0 || shader_kind != crate::model::glconst::GL_VERTEX_SHADER => {
                return Some(
                    "'attribute' : only vertex-shader global declarations may use this qualifier"
                        .into(),
                );
            }
            "uniform" | "varying" if depth != 0 => {
                return Some(format!(
                    "'{token}' : storage qualifier is not allowed in a local declaration"
                ));
            }
            "invariant"
                if matches!(
                    tokens.get(at + 1).map(String::as_str),
                    Some("attribute" | "uniform")
                ) =>
            {
                return Some(format!(
                    "'invariant' : may not qualify a {} declaration",
                    tokens[at + 1]
                ));
            }
            _ => {}
        }
    }
    None
}

/// Diagnose arithmetic whose operands require an implicit integer/floating-point conversion.
///
/// GLSL ES 1.00 section 5.9 permits scalar/vector arithmetic only when the operands have compatible
/// basic types; unlike later desktop GLSL, it supplies no implicit conversion between `int` and `float`.
/// This check runs at `glCompileShader`, where the API must expose a source-language error, instead of
/// waiting until a linked program eventually reaches the host backend.
pub fn invalid_implicit_arithmetic(source: &str) -> Option<String> {
    let tokens = tokens(source);
    let mut scopes = vec![HashMap::<&str, (&str, ScalarKind)>::new()];

    for at in 0..tokens.len() {
        match tokens[at].as_str() {
            "{" => {
                scopes.push(HashMap::new());
                continue;
            }
            "}" => {
                if scopes.len() > 1 {
                    scopes.pop();
                }
                continue;
            }
            _ => {}
        }

        if let (Some(kind), Some(name)) = (
            scalar_kind(&tokens[at]),
            tokens.get(at + 1).map(String::as_str),
        ) {
            if name
                .as_bytes()
                .first()
                .is_some_and(|byte| *byte == b'_' || byte.is_ascii_alphabetic())
            {
                scopes
                    .last_mut()
                    .expect("the global scope is never removed")
                    .insert(name, (tokens[at].as_str(), kind));
            }
        }

        let Some(operation) = tokens.get(at + 1).map(String::as_str) else {
            continue;
        };
        if !matches!(operation, "+" | "-" | "*" | "/") {
            continue;
        }
        // A component selector is not an identifier lookup. The tokenizer used to discard `.`, so the
        // `b` in `n.b + m` resolved to an unrelated global named `b`; if their scalar kinds differed, a
        // valid expression was refused before it ever reached the real compiler.
        if at > 0 && tokens[at - 1] == "." {
            continue;
        }
        let Some((left_type, left_kind)) = resolve(&scopes, tokens[at].as_str()) else {
            continue;
        };
        let Some(right) = tokens.get(at + 2) else {
            continue;
        };
        let Some((right_type, right_kind)) = resolve(&scopes, right) else {
            continue;
        };
        if left_kind != right_kind {
            return Some(format!(
                "'{operation}' : implicit conversion between '{left_type}' and '{right_type}' is not allowed in GLSL ES 1.00"
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::invalid_implicit_arithmetic;

    #[test]
    fn component_names_do_not_alias_unrelated_variables() {
        assert_eq!(
            invalid_implicit_arithmetic(
                "float r; int m; vec3 n; void main(){ int k = ivec2(1).r + m; float x = n.b + r; }"
            ),
            None
        );
        assert!(invalid_implicit_arithmetic(
            "float r; int m; void main(){ float x = r + m; }"
        )
        .is_some());
    }
}
