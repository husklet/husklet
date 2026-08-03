use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScalarKind {
    Float,
    Int,
}

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
    while at < bytes.len() {
        if bytes[at..].starts_with(b"//") {
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
            let start = at;
            at += 1;
            while at < bytes.len() && (bytes[at] == b'_' || bytes[at].is_ascii_alphanumeric()) {
                at += 1;
            }
            out.push(source[start..at].to_string());
        } else {
            if matches!(
                bytes[at],
                b'+' | b'-' | b'*' | b'/' | b';' | b'{' | b'}' | b'=' | b',' | b'(' | b')'
            ) {
                out.push((bytes[at] as char).to_string());
            }
            at += 1;
        }
    }
    out
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
