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

fn vector_width(token: &str) -> Option<usize> {
    match token {
        "vec2" | "ivec2" | "bvec2" => Some(2),
        "vec3" | "ivec3" | "bvec3" => Some(3),
        "vec4" | "ivec4" | "bvec4" => Some(4),
        _ => None,
    }
}

fn vector_scalar_type(token: &str) -> Option<&'static str> {
    match token {
        "vec2" | "vec3" | "vec4" => Some("float"),
        "ivec2" | "ivec3" | "ivec4" => Some("int"),
        "bvec2" | "bvec3" | "bvec4" => Some("bool"),
        _ => None,
    }
}

fn vector_type(scalar: &str, width: usize) -> Option<&'static str> {
    match (scalar, width) {
        ("float", 1) => Some("float"),
        ("float", 2) => Some("vec2"),
        ("float", 3) => Some("vec3"),
        ("float", 4) => Some("vec4"),
        ("int", 1) => Some("int"),
        ("int", 2) => Some("ivec2"),
        ("int", 3) => Some("ivec3"),
        ("int", 4) => Some("ivec4"),
        ("bool", 1) => Some("bool"),
        ("bool", 2) => Some("bvec2"),
        ("bool", 3) => Some("bvec3"),
        ("bool", 4) => Some("bvec4"),
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
            if let Some(operator) = [
                "<<=", ">>=", "%=", "&=", "^=", "|=", "+=", "-=", "*=", "/=", "++", "--",
            ]
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
                    | b'['
                    | b']'
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct Parameter {
    ty: String,
    qualifier: String,
    precision: String,
    is_const: bool,
    array: Option<Option<u32>>,
    name: String,
}

#[derive(Clone, Debug)]
struct Function {
    name: String,
    return_type: String,
    return_qualifiers: Vec<String>,
    parameters: Vec<Parameter>,
    body: Option<(usize, usize)>,
    declaration_at: usize,
}

fn matching(tokens: &[String], open: usize, left: &str, right: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (at, token) in tokens.iter().enumerate().skip(open) {
        if token == left {
            depth += 1;
        } else if token == right {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(at);
            }
        }
    }
    None
}

fn type_token(token: &str) -> bool {
    TYPES.contains(&token)
        || token.starts_with("sampler")
        || token.starts_with("isampler")
        || token.starts_with("usampler")
}

fn parameter(segment: &[String]) -> Option<Parameter> {
    if segment.is_empty() || segment == ["void"] {
        return None;
    }
    let mut qualifier = "in".to_string();
    let mut precision = String::new();
    let mut is_const = false;
    let mut ty = None;
    let mut name = None;
    let mut array = None;
    let mut at = 0;
    while at < segment.len() {
        match segment[at].as_str() {
            "in" | "out" | "inout" => qualifier = segment[at].clone(),
            "lowp" | "mediump" | "highp" => precision = segment[at].clone(),
            "const" => is_const = true,
            token if type_token(token) => ty = Some(segment[at].clone()),
            "[" => {
                let size = segment
                    .get(at + 1)
                    .and_then(|value| value.parse::<u32>().ok());
                array = Some(size);
            }
            token if is_identifier_token(token) => {
                if ty.is_none() {
                    ty = Some(segment[at].clone());
                } else {
                    name = Some(segment[at].clone());
                }
            }
            _ => {}
        }
        at += 1;
    }
    Some(Parameter {
        ty: ty?,
        qualifier,
        precision,
        is_const,
        array,
        name: name?,
    })
}

fn parameter_qualifier_order_is_valid(segment: &[String]) -> bool {
    let mut previous = 0u8;
    for rank in segment.iter().filter_map(|token| match token.as_str() {
        "const" => Some(1),
        "in" | "out" | "inout" => Some(2),
        "lowp" | "mediump" | "highp" => Some(3),
        _ => None,
    }) {
        if rank < previous {
            return false;
        }
        previous = rank;
    }
    true
}

fn parse_functions(source_tokens: &[String]) -> Result<Vec<Function>, String> {
    let mut functions = Vec::new();
    let mut brace_depth = 0usize;
    let mut at = 0usize;
    while at < source_tokens.len() {
        match source_tokens[at].as_str() {
            "{" => {
                brace_depth += 1;
                at += 1;
                continue;
            }
            "}" => {
                brace_depth = brace_depth.saturating_sub(1);
                at += 1;
                continue;
            }
            _ => {}
        }
        if source_tokens.get(at + 1).map(String::as_str) != Some("(")
            || !is_word_token(&source_tokens[at])
        {
            at += 1;
            continue;
        }
        let Some(close) = matching(source_tokens, at + 1, "(", ")") else {
            return Err(format!(
                "'{}' : unterminated parameter list",
                source_tokens[at]
            ));
        };
        if !matches!(
            source_tokens.get(close + 1).map(String::as_str),
            Some(";" | "{")
        ) {
            at = close + 1;
            continue;
        }

        // Calls can occur at every depth, but function declarations are global in GLSL ES.
        if brace_depth != 0 {
            if source_tokens.get(close + 1).map(String::as_str) == Some("{")
                || source_tokens.get(close + 1).map(String::as_str) == Some(";")
                    && at > 0
                    && type_token(&source_tokens[at - 1])
            {
                return Err(format!(
                    "'{}' : function declarations must be global",
                    source_tokens[at]
                ));
            }
            at = close + 1;
            continue;
        }

        let start = (0..at)
            .rev()
            .find(|&index| matches!(source_tokens[index].as_str(), ";" | "}" | "{"))
            .map_or(0, |index| index + 1);
        let prefix = &source_tokens[start..at];
        let Some(return_at) = prefix
            .iter()
            .rposition(|token| type_token(token))
            .or_else(|| {
                (!prefix.is_empty() && is_word_token(prefix.last().unwrap()))
                    .then(|| prefix.len() - 1)
            })
        else {
            return Err(format!(
                "'{}' : function is missing a return type",
                source_tokens[at]
            ));
        };
        if return_at + 1 != prefix.len() {
            at = close + 1;
            continue;
        }
        let return_type = prefix[return_at].clone();
        let return_qualifiers = prefix[..return_at].to_vec();
        if return_qualifiers
            .iter()
            .any(|q| matches!(q.as_str(), "uniform" | "varying" | "attribute"))
        {
            return Err(format!(
                "'{}' : storage qualifier is not valid on a function return type",
                source_tokens[at]
            ));
        }
        let mut parameters = Vec::new();
        let mut segment_start = at + 2;
        for split in (at + 2..=close).filter(|&index| index == close || source_tokens[index] == ",")
        {
            let segment = &source_tokens[segment_start..split];
            if !segment.is_empty() && segment != ["void"] {
                if !parameter_qualifier_order_is_valid(segment) {
                    return Err(format!(
                        "'{}' : function parameter qualifiers are out of order",
                        source_tokens[at]
                    ));
                }
                let parsed = parameter(segment).ok_or_else(|| {
                    format!(
                        "'{}' : every function parameter requires a type and name",
                        source_tokens[at]
                    )
                })?;
                if segment
                    .iter()
                    .any(|q| matches!(q.as_str(), "uniform" | "varying" | "attribute"))
                {
                    return Err(format!(
                        "'{}' : storage qualifier is not valid on a function parameter",
                        source_tokens[at]
                    ));
                }
                parameters.push(parsed);
            }
            segment_start = split + 1;
        }
        let body = if source_tokens.get(close + 1).map(String::as_str) == Some("{") {
            let end = matching(source_tokens, close + 1, "{", "}")
                .ok_or_else(|| format!("'{}' : unterminated function body", source_tokens[at]))?;
            Some((close + 2, end))
        } else {
            None
        };
        functions.push(Function {
            name: source_tokens[at].clone(),
            return_type,
            return_qualifiers,
            parameters,
            body,
            declaration_at: at,
        });
        at = body.map_or(close + 2, |(_, end)| end + 1);
    }
    Ok(functions)
}

fn overload_key(function: &Function) -> Vec<(&str, Option<Option<u32>>)> {
    function
        .parameters
        .iter()
        .map(|parameter| (parameter.ty.as_str(), parameter.array))
        .collect()
}

/// Validate GLSL ES function declarations, definitions and calls at `glCompileShader`.
///
/// The host compiler cannot be the API compile boundary: shader IR is intentionally deferred until a
/// draw. This parser therefore owns the source-language rules whose meaning would otherwise be erased by
/// the ES-to-desktop rewrite (notably parameter qualifiers and declaration order).
pub fn invalid_function_semantics(source: &str) -> Option<String> {
    let source_tokens = tokens(source);
    let functions = match parse_functions(&source_tokens) {
        Ok(functions) => functions,
        Err(reason) => return Some(reason),
    };
    let structs_with_arrays = source_tokens
        .windows(2)
        .enumerate()
        .filter_map(|(at, pair)| (pair[0] == "struct").then_some((at, pair[1].as_str())))
        .filter_map(|(at, name)| {
            let open = source_tokens[at..].iter().position(|token| token == "{")? + at;
            let close = matching(&source_tokens, open, "{", "}")?;
            source_tokens[open..close]
                .iter()
                .any(|token| token == "[")
                .then_some(name)
        })
        .collect::<Vec<_>>();
    let struct_names = source_tokens
        .windows(2)
        .filter_map(|pair| (pair[0] == "struct").then_some(pair[1].as_str()))
        .collect::<Vec<_>>();

    let mut seen: Vec<&Function> = Vec::new();
    for function in &functions {
        if function.name == "main"
            && (function.return_type != "void" || !function.parameters.is_empty())
        {
            return Some(
                "'main' : entry point must have return type void and no parameters".into(),
            );
        }
        if function.body.is_none()
            && function
                .parameters
                .iter()
                .any(|parameter| parameter.array == Some(None))
        {
            return Some(format!(
                "'{}' : an array parameter prototype requires an explicit size",
                function.name
            ));
        }
        if structs_with_arrays.contains(&function.return_type.as_str()) {
            return Some(format!(
                "'{}' : a structure containing an array cannot be returned",
                function.name
            ));
        }
        if struct_names.contains(&function.name.as_str()) {
            return Some(format!(
                "'{}' : function name conflicts with a structure type",
                function.name
            ));
        }
        let same_overload = seen
            .iter()
            .copied()
            .filter(|previous| {
                previous.name == function.name && overload_key(previous) == overload_key(function)
            })
            .collect::<Vec<_>>();
        if !same_overload.is_empty() {
            let same_contract =
                same_overload.iter().all(|previous| {
                    previous.return_type == function.return_type
                        && previous.return_qualifiers == function.return_qualifiers
                        && previous.parameters.len() == function.parameters.len()
                        && previous.parameters.iter().zip(&function.parameters).all(
                            |(left, right)| {
                                left.ty == right.ty
                                    && left.qualifier == right.qualifier
                                    && left.precision == right.precision
                                    && left.is_const == right.is_const
                                    && left.array == right.array
                            },
                        )
                });
            if !same_contract
                || function.body.is_some()
                    && same_overload.iter().any(|previous| previous.body.is_some())
                || function.body.is_none()
            {
                return Some(format!(
                    "'{}' : conflicting or duplicate function declaration",
                    function.name
                ));
            }
        }
        seen.push(function);

        let Some((body_start, body_end)) = function.body else {
            continue;
        };
        let body = &source_tokens[body_start..body_end];
        for open in 2..body.len() {
            if body[open] != "(" || !type_token(&body[open - 2]) || !is_word_token(&body[open - 1])
            {
                continue;
            }
            if let Some(close) = matching(body, open, "(", ")") {
                if matches!(body.get(close + 1).map(String::as_str), Some(";" | "{")) {
                    return Some(format!(
                        "'{}' : function declarations must be global",
                        body[open - 1]
                    ));
                }
            }
        }
        if function.return_type == "void" {
            if body
                .windows(2)
                .any(|tokens| tokens[0] == "return" && tokens[1] != ";")
            {
                return Some(format!(
                    "'{}' : a void function cannot return a value",
                    function.name
                ));
            }
        } else if body.windows(2).any(|tokens| tokens == ["return", ";"]) {
            return Some(format!(
                "'{}' : a non-void function must return a value",
                function.name
            ));
        }
        if body.iter().any(|token| token == "break")
            && !body
                .iter()
                .any(|token| matches!(token.as_str(), "for" | "while" | "do"))
        {
            return Some("'break' : statement is not enclosed by a loop".into());
        }
        if body.iter().any(|token| token == "continue")
            && !body
                .iter()
                .any(|token| matches!(token.as_str(), "for" | "while" | "do"))
        {
            return Some("'continue' : statement is not enclosed by a loop".into());
        }
        for parameter in function
            .parameters
            .iter()
            .filter(|parameter| parameter.is_const)
        {
            for use_at in 0..body.len() {
                if body[use_at] != parameter.name {
                    continue;
                }
                if matches!(
                    body.get(use_at + 1).map(String::as_str),
                    Some("=" | "+=" | "-=" | "*=" | "/=" | "++" | "--")
                ) || use_at > 0 && matches!(body[use_at - 1].as_str(), "++" | "--")
                    || body.get(use_at + 1).map(String::as_str) == Some("[")
                        && body[use_at + 1..].iter().take(5).any(|token| token == "=")
                {
                    return Some(format!(
                        "'{}' : const parameter is read-only",
                        parameter.name
                    ));
                }
                if use_at > 0 && body[use_at - 1] == "[" {
                    return Some(format!(
                        "'{}' : a parameter is not a constant expression",
                        parameter.name
                    ));
                }
                if body[..use_at]
                    .iter()
                    .rev()
                    .take(6)
                    .any(|token| token == "const")
                    && body[..use_at]
                        .iter()
                        .rev()
                        .take(3)
                        .any(|token| token == "=")
                {
                    return Some(format!(
                        "'{}' : a const parameter is not a constant expression",
                        parameter.name
                    ));
                }
            }
        }
    }

    // Validate user-function calls against declarations visible at the call site. Constructors and the
    // declaration parentheses themselves are excluded structurally.
    for at in 0..source_tokens.len().saturating_sub(1) {
        if source_tokens[at + 1] != "(" {
            continue;
        }
        let name = source_tokens[at].as_str();
        if type_token(name) || matches!(name, "if" | "for" | "while") {
            continue;
        }
        if functions
            .iter()
            .any(|function| function.declaration_at == at)
        {
            continue;
        }
        let candidates = functions
            .iter()
            .filter(|function| function.name == name && function.declaration_at < at)
            .collect::<Vec<_>>();
        if functions.iter().any(|function| function.name == name) && candidates.is_empty() {
            return Some(format!(
                "'{name}' : function must be declared before it is called"
            ));
        }
        if candidates.is_empty() {
            continue;
        }
        let Some(close) = matching(&source_tokens, at + 1, "(", ")") else {
            continue;
        };
        let arguments = if close == at + 2 {
            0
        } else {
            let mut nested = 0usize;
            let mut count = 1usize;
            for token in &source_tokens[at + 2..close] {
                match token.as_str() {
                    "(" | "[" => nested += 1,
                    ")" | "]" => nested = nested.saturating_sub(1),
                    "," if nested == 0 => count += 1,
                    _ => {}
                }
            }
            count
        };
        if !candidates
            .iter()
            .any(|function| function.parameters.len() == arguments)
        {
            return Some(format!(
                "'{name}' : no overload accepts {arguments} arguments"
            ));
        }
        let mut segments = Vec::new();
        let mut start = at + 2;
        let mut nested = 0usize;
        for index in at + 2..=close {
            match source_tokens[index].as_str() {
                "(" | "[" => nested += 1,
                ")" | "]" if index != close => nested = nested.saturating_sub(1),
                "," if nested == 0 => {
                    segments.push(&source_tokens[start..index]);
                    start = index + 1;
                }
                _ => {}
            }
        }
        if start < close {
            segments.push(&source_tokens[start..close]);
        }
        let declarations = source_tokens
            .windows(2)
            .enumerate()
            .filter_map(|(index, pair)| {
                if !type_token(&pair[0]) || !is_identifier_token(&pair[1]) {
                    return None;
                }
                let array =
                    (source_tokens.get(index + 2).map(String::as_str) == Some("[")).then(|| {
                        source_tokens
                            .get(index + 3)
                            .and_then(|size| size.parse::<u32>().ok())
                    });
                Some((pair[1].as_str(), (pair[0].as_str(), array)))
            })
            .collect::<HashMap<_, _>>();
        let inferred = segments
            .iter()
            .map(|segment| {
                let first = segment.first()?.as_str();
                if type_token(first) {
                    return Some((first, None));
                }
                if first.parse::<i64>().is_ok() {
                    return Some((
                        if segment.iter().any(|token| token == ".") {
                            "float"
                        } else {
                            "int"
                        },
                        None,
                    ));
                }
                if first.parse::<f64>().is_ok() {
                    return Some(("float", None));
                }
                let (mut ty, mut array) = declarations.get(first).copied()?;
                let mut index = 1;
                while index < segment.len() {
                    match segment[index].as_str() {
                        "[" => {
                            let close = matching(segment, index, "[", "]")?;
                            if array.is_some() {
                                array = None;
                            } else {
                                ty = vector_scalar_type(ty)?;
                            }
                            index = close + 1;
                        }
                        "." => {
                            let swizzle = segment.get(index + 1)?;
                            ty = vector_type(vector_scalar_type(ty)?, swizzle.len())?;
                            array = None;
                            index += 2;
                        }
                        _ => return None,
                    }
                }
                Some((ty, array))
            })
            .collect::<Vec<_>>();
        if !candidates.iter().any(|function| {
            function.parameters.len() == inferred.len()
                && function
                    .parameters
                    .iter()
                    .zip(&inferred)
                    .all(|(parameter, argument)| {
                        argument.is_none_or(|(ty, array)| {
                            ty == parameter.ty && array == parameter.array
                        })
                    })
        }) {
            return Some(format!("'{name}' : no overload matches the argument types"));
        }
    }
    None
}

/// Diagnose vector constructors that do not supply the destination component count.
/// A single scalar is the specified splat form. Otherwise arguments are consumed from left to
/// right until the vector is full. The final used argument may have unconsumed components, but a
/// later wholly unused argument is an error. A smaller vector alone is not implicitly padded.
pub fn invalid_vector_constructor(source: &str) -> Option<String> {
    let source_tokens = tokens(source);
    let declarations = source_tokens
        .windows(2)
        .filter_map(|pair| {
            if !is_word_token(&pair[1]) {
                return None;
            }
            vector_width(&pair[0])
                .or_else(|| matches!(pair[0].as_str(), "float" | "int" | "bool").then_some(1))
                .map(|width| (pair[1].as_str(), width))
        })
        .collect::<HashMap<_, _>>();
    for at in 0..source_tokens.len().saturating_sub(2) {
        let Some(target_width) = vector_width(&source_tokens[at]) else {
            continue;
        };
        if source_tokens[at + 1] != "(" {
            continue;
        }
        let Some(close) = matching(&source_tokens, at + 1, "(", ")") else {
            continue;
        };
        let mut arguments = Vec::new();
        let mut start = at + 2;
        let mut nested = 0usize;
        for index in at + 2..=close {
            match source_tokens[index].as_str() {
                "(" | "[" => nested += 1,
                ")" | "]" if index != close => nested = nested.saturating_sub(1),
                "," if nested == 0 => {
                    arguments.push(&source_tokens[start..index]);
                    start = index + 1;
                }
                _ => {}
            }
        }
        if start < close {
            arguments.push(&source_tokens[start..close]);
        }
        let widths = arguments
            .iter()
            .map(|argument| {
                let first = argument.first()?;
                if argument.len() >= 2 && argument[argument.len() - 2] == "." {
                    let swizzle = argument.last()?;
                    if swizzle
                        .chars()
                        .all(|component| "xyzwrgba".contains(component) || "stpq".contains(component))
                    {
                        return Some(swizzle.len());
                    }
                }
                if let Some(width) = vector_width(first) {
                    if argument.get(1).map(String::as_str) == Some("(") {
                        let close = matching(argument, 1, "(", ")")?;
                        if close + 1 < argument.len() {
                            return None;
                        }
                    }
                    return Some(width);
                }
                if first.parse::<f64>().is_ok() || matches!(first.as_str(), "true" | "false") {
                    return Some(1);
                }
                if argument.get(1).map(String::as_str) == Some(".") {
                    return argument.get(2).map(|swizzle| swizzle.len());
                }
                (argument.len() == 1)
                    .then(|| declarations.get(first.as_str()).copied())
                    .flatten()
            })
            .collect::<Option<Vec<_>>>();
        let Some(widths) = widths else { continue };
        let total = widths.iter().sum::<usize>();
        let before_last = widths[..widths.len().saturating_sub(1)]
            .iter()
            .sum::<usize>();
        let valid = widths == [1] || (before_last < target_width && total >= target_width);
        if !valid {
            return Some(format!(
                "'{}' : constructor supplies {} components but requires {target_width}",
                source_tokens[at],
                widths.iter().sum::<usize>()
            ));
        }
    }
    None
}

#[derive(Clone, Debug)]
struct Variable {
    name: String,
    declaration: usize,
    visible_from: usize,
    visible_until: usize,
    scope: usize,
}

fn brace_ranges(source_tokens: &[String]) -> Vec<(usize, usize)> {
    let mut stack = Vec::new();
    let mut ranges = Vec::new();
    for (at, token) in source_tokens.iter().enumerate() {
        if token == "{" {
            stack.push(at);
        } else if token == "}" {
            if let Some(open) = stack.pop() {
                ranges.push((open, at));
            }
        }
    }
    ranges
}

fn enclosing_range(ranges: &[(usize, usize)], at: usize) -> Option<(usize, usize)> {
    ranges
        .iter()
        .copied()
        .filter(|(open, close)| *open < at && at < *close)
        .min_by_key(|(open, close)| close - open)
}

fn statement_end(source_tokens: &[String], at: usize) -> usize {
    source_tokens[at..]
        .iter()
        .position(|token| token == ";")
        .map_or(source_tokens.len(), |offset| at + offset)
}

fn is_builtin_function(name: &str) -> bool {
    matches!(
        name,
        "radians"
            | "degrees"
            | "sin"
            | "cos"
            | "tan"
            | "asin"
            | "acos"
            | "atan"
            | "pow"
            | "exp"
            | "log"
            | "exp2"
            | "log2"
            | "sqrt"
            | "inversesqrt"
            | "abs"
            | "sign"
            | "floor"
            | "ceil"
            | "fract"
            | "mod"
            | "min"
            | "max"
            | "clamp"
            | "mix"
            | "step"
            | "smoothstep"
            | "length"
            | "distance"
            | "dot"
            | "cross"
            | "normalize"
            | "faceforward"
            | "reflect"
            | "refract"
            | "matrixCompMult"
            | "lessThan"
            | "lessThanEqual"
            | "greaterThan"
            | "greaterThanEqual"
            | "equal"
            | "notEqual"
            | "any"
            | "all"
            | "not"
            | "texture2D"
            | "texture2DProj"
            | "texture2DLod"
            | "texture2DProjLod"
            | "textureCube"
            | "textureCubeLod"
            | "dFdx"
            | "dFdy"
            | "fwidth"
    )
}

/// Validate lexical scopes and the single ordinary-identifier namespace required by GLSL ES 1.00.
///
/// This deliberately models declaration visibility intervals rather than searching source spelling:
/// braces, function bodies, loop scopes and single-statement branches each own a lifetime. As a result a
/// legal shadow in a child block remains distinct from a redeclaration in the same scope.
pub fn invalid_scope_semantics(source: &str) -> Option<String> {
    let source_tokens = tokens(source);
    let ranges = brace_ranges(&source_tokens);
    let functions = parse_functions(&source_tokens).ok()?;
    let structs = source_tokens
        .windows(2)
        .enumerate()
        .filter_map(|(at, pair)| (pair[0] == "struct").then_some((pair[1].clone(), at + 1)))
        .collect::<Vec<_>>();
    let struct_ranges = structs
        .iter()
        .filter_map(|(_, at)| {
            let open = source_tokens[*at..].iter().position(|token| token == "{")? + at;
            Some((open, matching(&source_tokens, open, "{", "}")?))
        })
        .collect::<Vec<_>>();
    let loops = source_tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| matches!(token.as_str(), "for" | "while"))
        .filter_map(|(at, _)| {
            let open =
                (source_tokens.get(at + 1).map(String::as_str) == Some("(")).then_some(at + 1)?;
            let condition_end = matching(&source_tokens, open, "(", ")")?;
            let body_start = condition_end + 1;
            let body_end = if source_tokens.get(body_start).map(String::as_str) == Some("{") {
                matching(&source_tokens, body_start, "{", "}")?
            } else {
                statement_end(&source_tokens, body_start)
            };
            Some((at, open, condition_end, body_start, body_end))
        })
        .collect::<Vec<_>>();

    for function in &functions {
        if is_builtin_function(&function.name) {
            return Some(format!(
                "'{}' : built-in functions may not be redeclared",
                function.name
            ));
        }
    }
    for (name, declaration) in &structs {
        if source_tokens[..*declaration]
            .iter()
            .enumerate()
            .any(|(at, token)| {
                token == name
                    && source_tokens.get(at + 1).map(String::as_str) == Some("(")
                    && source_tokens.get(at.wrapping_sub(1)).map(String::as_str) != Some("struct")
            })
        {
            return Some(format!(
                "'{name}' : structure type used before its declaration"
            ));
        }
    }

    let parameter_names = functions
        .iter()
        .flat_map(|function| {
            function
                .parameters
                .iter()
                .map(|parameter| parameter.name.as_str())
        })
        .collect::<std::collections::HashSet<_>>();
    let parameter_name_indices = functions
        .iter()
        .flat_map(|function| {
            let open = function.declaration_at + 1;
            let close = matching(&source_tokens, open, "(", ")").unwrap_or(open);
            (open + 1..close).filter(|at| parameter_names.contains(source_tokens[*at].as_str()))
        })
        .collect::<std::collections::HashSet<_>>();

    let mut variables = Vec::new();
    for at in 0..source_tokens.len().saturating_sub(1) {
        let ty = source_tokens[at].as_str();
        if ty == "void"
            || !(type_token(ty) || structs.iter().any(|(name, _)| name == ty))
            || !is_identifier_token(&source_tokens[at + 1])
            || source_tokens.get(at + 2).map(String::as_str) == Some("(")
            || parameter_name_indices.contains(&(at + 1))
            || struct_ranges
                .iter()
                .any(|(open, close)| *open < at && at < *close)
        {
            continue;
        }
        let declaration = at + 1;
        let (mut scope, mut visible_until) = enclosing_range(&ranges, declaration)
            .map_or((usize::MAX, source_tokens.len()), |(open, close)| {
                (open, close)
            });
        let end = statement_end(&source_tokens, declaration);
        let mut visible_from = end.saturating_add(1);
        if let Some((loop_at, _, condition_end, _, loop_end)) =
            loops.iter().find(|(_, open, condition_end, _, _)| {
                *open < declaration && declaration < *condition_end
            })
        {
            scope = *loop_at;
            visible_until = *loop_end;
            if source_tokens[*loop_at] == "while" {
                visible_from = condition_end.saturating_add(1);
            }
        }

        // A declaration controlled by an unbraced branch is scoped to that one statement.
        let boundary = (0..at)
            .rev()
            .find(|index| matches!(source_tokens[*index].as_str(), ";" | "{" | "}"));
        let statement_start = boundary.map_or(0, |index| index + 1);
        if source_tokens[statement_start..at]
            .iter()
            .any(|token| token == "else")
            || source_tokens[statement_start..at]
                .iter()
                .any(|token| token == "if")
                && !source_tokens[statement_start..at]
                    .iter()
                    .any(|token| token == "{")
        {
            scope = statement_start;
            visible_until = end;
        }
        variables.push(Variable {
            name: source_tokens[declaration].clone(),
            declaration,
            visible_from,
            visible_until,
            scope,
        });
        let mut nested = 0usize;
        for comma in declaration + 1..end {
            match source_tokens[comma].as_str() {
                "(" | "[" => nested += 1,
                ")" | "]" => nested = nested.saturating_sub(1),
                "," if nested == 0 => {
                    let Some(name_at) =
                        (comma + 1..end).find(|index| is_identifier_token(&source_tokens[*index]))
                    else {
                        continue;
                    };
                    variables.push(Variable {
                        name: source_tokens[name_at].clone(),
                        declaration: name_at,
                        visible_from: end.saturating_add(1),
                        visible_until,
                        scope,
                    });
                }
                _ => {}
            }
        }
    }

    // Function parameters live in the definition body, not in a preceding prototype and not globally.
    for function in &functions {
        let Some((body_start, body_end)) = function.body else {
            continue;
        };
        // Parameters form the function-declaration scope. The outermost compound statement is a child
        // scope, so GLSL ES permits a local declaration there to hide a parameter.
        let body_scope = function.declaration_at;
        for parameter in &function.parameters {
            variables.push(Variable {
                name: parameter.name.clone(),
                declaration: function.declaration_at,
                visible_from: body_start,
                visible_until: body_end,
                scope: body_scope,
            });
        }
    }
    for prototype in functions.iter().filter(|function| function.body.is_none()) {
        let Some(definition) = functions.iter().find(|function| {
            function.body.is_some()
                && function.name == prototype.name
                && overload_key(function) == overload_key(prototype)
        }) else {
            continue;
        };
        let (body_start, body_end) = definition.body.unwrap();
        for parameter in &prototype.parameters {
            if definition
                .parameters
                .iter()
                .any(|defined| defined.name == parameter.name)
            {
                continue;
            }
            if source_tokens[body_start..body_end]
                .iter()
                .any(|token| token == &parameter.name)
            {
                return Some(format!(
                    "'{}' : prototype parameter names are not visible in the definition",
                    parameter.name
                ));
            }
        }
    }

    for (index, left) in variables.iter().enumerate() {
        if variables[..index]
            .iter()
            .any(|right| right.name == left.name && right.scope == left.scope)
        {
            return Some(format!(
                "'{}' : redeclared in the same lexical scope",
                left.name
            ));
        }
        if left.scope == usize::MAX && functions.iter().any(|function| function.name == left.name) {
            return Some(format!(
                "'{}' : variable conflicts with a function",
                left.name
            ));
        }
        if let Some((_, _, _, body_start, body_end)) = loops
            .iter()
            .find(|(loop_at, _, _, _, _)| *loop_at == left.scope)
        {
            if variables.iter().any(|right| {
                right.name == left.name
                    && *body_start < right.declaration
                    && right.declaration < *body_end
            }) {
                return Some(format!(
                    "'{}' : loop variable redeclared in its body",
                    left.name
                ));
            }
        }
    }

    let declared_names = variables
        .iter()
        .map(|variable| variable.name.as_str())
        .collect::<std::collections::HashSet<_>>();
    for (at, token) in source_tokens.iter().enumerate() {
        if !declared_names.contains(token.as_str())
            || variables.iter().any(|variable| variable.declaration == at)
            || variables
                .iter()
                .any(|variable| variable.declaration == at.saturating_add(1))
            || parameter_name_indices.contains(&at)
            || structs.iter().any(|(_, declaration)| *declaration == at)
            || at > 0 && source_tokens[at - 1] == "."
            || source_tokens.get(at + 1).map(String::as_str) == Some("(")
            || struct_ranges
                .iter()
                .any(|(open, close)| *open < at && at < *close)
        {
            continue;
        }
        if !variables.iter().any(|variable| {
            variable.name == *token && variable.visible_from <= at && at < variable.visible_until
        }) {
            return Some(format!(
                "'{token}' : identifier is outside its lexical scope"
            ));
        }
    }
    None
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

fn is_identifier_token(token: &str) -> bool {
    token
        .as_bytes()
        .first()
        .is_some_and(|byte| *byte == b'_' || byte.is_ascii_alphabetic())
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
            "varying" => {
                let mut type_at = at + 1;
                while matches!(
                    tokens.get(type_at).map(String::as_str),
                    Some("lowp" | "mediump" | "highp")
                ) {
                    type_at += 1;
                }
                if !matches!(
                    tokens.get(type_at).map(String::as_str),
                    Some("float" | "vec2" | "vec3" | "vec4" | "mat2" | "mat3" | "mat4")
                ) {
                    return Some(format!(
                        "'{}' : GLSL ES 1.00 varyings must have a floating-point type",
                        tokens
                            .get(type_at)
                            .map(String::as_str)
                            .unwrap_or("<missing>")
                    ));
                }
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

/// Diagnose an ES 1.00 call whose scalar argument has a different basic type from its parameter.
pub fn invalid_function_argument_basetype(source: &str) -> Option<String> {
    let tokens = tokens(source);
    let mut functions = HashMap::<&str, (&str, ScalarKind)>::new();
    for at in 0..tokens.len().saturating_sub(6) {
        let Some(parameter_kind) = tokens.get(at + 3).and_then(|token| scalar_kind(token)) else {
            continue;
        };
        if TYPES.contains(&tokens[at].as_str())
            && is_word_token(&tokens[at + 1])
            && tokens[at + 2] == "("
            && is_word_token(&tokens[at + 4])
            && tokens[at + 5] == ")"
            && tokens[at + 6] == "{"
        {
            functions.insert(&tokens[at + 1], (&tokens[at + 3], parameter_kind));
        }
    }
    for at in 0..tokens.len().saturating_sub(3) {
        let Some(&(parameter_type, parameter_kind)) = functions.get(tokens[at].as_str()) else {
            continue;
        };
        if tokens[at + 1] != "(" || tokens[at + 3] != ")" {
            continue;
        }
        // Skip the function definition itself.
        if at > 0 && TYPES.contains(&tokens[at - 1].as_str()) {
            continue;
        }
        let argument = tokens[at + 2].as_str();
        let argument_kind = if argument.as_bytes().iter().all(u8::is_ascii_digit) {
            Some(ScalarKind::Int)
        } else if argument.parse::<f64>().is_ok() {
            Some(ScalarKind::Float)
        } else {
            None
        };
        if argument_kind.is_some_and(|kind| kind != parameter_kind) {
            return Some(format!(
                "'{}' : cannot convert argument to parameter type '{parameter_type}' in GLSL ES 1.00",
                tokens[at]
            ));
        }
    }
    None
}

/// ES 1.00 forbids statically writing both legacy fragment-output interfaces in one shader, including
/// writes in mutually exclusive branches or an otherwise unused function.
pub fn invalid_fragment_output_mix(source: &str) -> Option<String> {
    let source_tokens = tokens(source);
    (source_tokens.iter().any(|token| token == "gl_FragColor")
        && source_tokens.iter().any(|token| token == "gl_FragData"))
    .then(|| "a fragment shader may not statically write both gl_FragColor and gl_FragData".into())
}

#[cfg(test)]
mod tests {
    use super::invalid_implicit_arithmetic;

    #[test]
    fn es100_rejects_non_float_varying_types() {
        for source in [
            "varying bool value; void main(){}",
            "varying mediump int value; void main(){}",
            "varying bvec3 value; void main(){}",
            "varying ivec2 value; void main(){}",
            "struct Value { float x; }; varying Value value; void main(){}",
        ] {
            assert!(
                super::invalid_storage_declaration(source, crate::model::glconst::GL_VERTEX_SHADER)
                    .is_some(),
                "invalid varying compiled: {source}"
            );
        }
        assert!(super::invalid_storage_declaration(
            "varying mediump vec3 value; void main(){}",
            crate::model::glconst::GL_VERTEX_SHADER
        )
        .is_none());
    }

    #[test]
    fn component_names_do_not_alias_unrelated_variables() {
        assert_eq!(
            invalid_implicit_arithmetic(
                "float r; int m; vec3 n; void main(){ int k = ivec2(1).r + m; float x = n.b + r; }"
            ),
            None
        );
        assert!(
            invalid_implicit_arithmetic("float r; int m; void main(){ float x = r + m; }")
                .is_some()
        );
    }
}
