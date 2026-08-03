use super::{Scope, Token, Type};

/// Token-stream operations that recover declaration and scope structure from accepted GLSL.
pub(super) struct TokenStream;

impl TokenStream {
    pub(super) fn tokenize(source: &str) -> Vec<Token> {
        let bytes = source.as_bytes();
        let mut out = Vec::new();
        let mut at = 0usize;
        while at < bytes.len() {
            if bytes[at].is_ascii_whitespace() {
                at += 1;
                continue;
            }
            if bytes[at] == b'/' && bytes.get(at + 1) == Some(&b'/') {
                at += 2;
                while at < bytes.len() && bytes[at] != b'\n' {
                    at += 1;
                }
                continue;
            }
            if bytes[at] == b'/' && bytes.get(at + 1) == Some(&b'*') {
                at += 2;
                while at + 1 < bytes.len() && &bytes[at..at + 2] != b"*/" {
                    at += 1;
                }
                at = (at + 2).min(bytes.len());
                continue;
            }
            let start = at;
            if bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_' {
                while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                    at += 1;
                }
            } else {
                at += 1;
            }
            out.push(Token {
                text: source[start..at].to_owned(),
                start,
                end: at,
            });
        }
        out
    }

    pub(super) fn scopes(tokens: &[Token], source_end: usize) -> (Vec<Scope>, Vec<usize>) {
        let mut scopes = vec![Scope {
            start: 0,
            end: source_end,
            parent: None,
        }];
        let mut stack = vec![0usize];
        let mut token_scopes = Vec::with_capacity(tokens.len());
        for token in tokens {
            if token.text == "{" {
                let parent = *stack.last().unwrap_or(&0);
                let index = scopes.len();
                scopes.push(Scope {
                    start: token.start,
                    end: source_end,
                    parent: Some(parent),
                });
                stack.push(index);
            }
            token_scopes.push(*stack.last().unwrap_or(&0));
            if token.text == "}" && stack.len() > 1 {
                let scope = stack.pop().unwrap();
                scopes[scope].end = token.end;
            }
        }
        (scopes, token_scopes)
    }

    pub(super) fn matching(
        tokens: &[Token],
        open: usize,
        left: &str,
        right: &str,
    ) -> Option<usize> {
        let mut depth = 0usize;
        for (at, token) in tokens.iter().enumerate().skip(open) {
            if token.text == left {
                depth += 1;
            }
            if token.text == right {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at);
                }
            }
        }
        None
    }

    pub(super) fn inside(at: usize, ranges: &[(usize, usize)]) -> bool {
        ranges.iter().any(|(start, end)| *start <= at && at <= *end)
    }

    pub(super) fn declaration_context(tokens: &[Token], type_at: usize) -> bool {
        let mut before = type_at;
        while before > 0 && tokens[before - 1].qualifier() {
            before -= 1;
        }
        before == 0
            || matches!(
                tokens[before - 1].text.as_str(),
                "{" | "}" | ";" | "(" | "," | ":"
            )
    }

    pub(super) fn function_parameter(tokens: &[Token], at: usize) -> bool {
        let mut depth = 0usize;
        for cursor in (0..at).rev() {
            match tokens[cursor].text.as_str() {
                ")" => depth += 1,
                "(" if depth != 0 => depth -= 1,
                "(" => {
                    return cursor > 1
                        && tokens[cursor - 1].identifier()
                        && tokens[cursor - 2].identifier()
                        && !tokens[cursor - 2].qualifier();
                }
                ";" | "{" | "}" if depth == 0 => return false,
                _ => {}
            }
        }
        false
    }

    pub(super) fn next_type(
        tokens: &[Token],
        mut at: usize,
        known: impl Fn(&str) -> bool,
    ) -> Option<usize> {
        while at < tokens.len() {
            if known(&tokens[at].text) {
                return Some(at);
            }
            at += 1;
        }
        None
    }

    pub(super) fn previous_type(
        tokens: &[Token],
        mut at: usize,
        known: impl Fn(&str) -> bool,
    ) -> Option<usize> {
        while at > 0 {
            at -= 1;
            if known(&tokens[at].text) {
                return Some(at);
            }
            if !tokens[at].qualifier() {
                return None;
            }
        }
        None
    }

    pub(super) fn statement_end(tokens: &[Token], start: usize) -> usize {
        let mut depth = 0usize;
        for (at, token) in tokens.iter().enumerate().skip(start) {
            match token.text.as_str() {
                "(" | "[" => depth += 1,
                ")" | "]" => depth = depth.saturating_sub(1),
                ";" if depth == 0 => return at,
                "{" | "}" if depth == 0 && at != start => return at.saturating_sub(1),
                _ => {}
            }
        }
        tokens.len().saturating_sub(1)
    }

    pub(super) fn declaration(
        tokens: &[Token],
        start: usize,
        end: usize,
        known: impl Fn(&str) -> bool,
    ) -> Option<(Type, Vec<(String, usize)>)> {
        let type_at = (start..end).find(|at| known(&tokens[*at].text))?;
        let ty = Type::named(&tokens[type_at].text);
        let mut names = Vec::new();
        let mut at = type_at + 1;
        while at < end {
            if !tokens[at].identifier() || tokens.get(at + 1).is_some_and(|token| token.text == "(")
            {
                return None;
            }
            let name = tokens[at].text.clone();
            at += 1;
            let mut array_depth = 0;
            while tokens.get(at).is_some_and(|token| token.text == "[") {
                at = Self::matching(tokens, at, "[", "]")? + 1;
                array_depth += 1;
            }
            names.push((name, array_depth));

            let mut depth = 0usize;
            while at < end {
                match tokens[at].text.as_str() {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => depth = depth.saturating_sub(1),
                    "," if depth == 0 => {
                        at += 1;
                        break;
                    }
                    _ => {}
                }
                at += 1;
            }
        }
        (!names.is_empty()).then_some((ty, names))
    }

    pub(super) fn parameter_declarations(
        tokens: &[Token],
        start: usize,
        end: usize,
        known: impl Fn(&str) -> bool + Copy,
    ) -> Vec<(String, Type)> {
        let mut out = Vec::new();
        let mut segment = start;
        for at in start..=end {
            if at == end || tokens[at].text == "," {
                if let Some((ty, names)) = Self::declaration(tokens, segment, at, known) {
                    if let Some((name, array_depth)) = names.into_iter().next() {
                        out.push((name, ty.arrays(array_depth)));
                    }
                }
                segment = at + 1;
            }
        }
        out
    }
}
