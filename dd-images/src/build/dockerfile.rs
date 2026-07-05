//! Dockerfile lexing/parsing: split a Dockerfile into instructions, expand build-args, and parse the
//! exec/shell forms of `CMD`/`ENTRYPOINT` and the modern/legacy forms of `LABEL`. All pure functions —
//! no filesystem, no runtime — so the daemon (or any caller) can drive its own build loop over them.

use serde_json::Value;
use std::collections::HashMap;

/// Substitute `${NAME}` / `$NAME` references in a Dockerfile line using the merged ARG map.
/// Unknown `${NAME}` expands to empty (like docker); unknown `$NAME` is left literal.
pub fn substitute_args(s: &str, map: &HashMap<String, String>) -> String {
    if map.is_empty() || !s.contains('$') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('{') => {
                chars.next(); // consume '{'
                let mut name = String::new();
                while let Some(&nc) = chars.peek() {
                    chars.next();
                    if nc == '}' {
                        break;
                    }
                    name.push(nc);
                }
                if let Some(v) = map.get(&name) {
                    out.push_str(v);
                }
            }
            Some(nc) if nc.is_ascii_alphanumeric() || nc == '_' => {
                let mut name = String::new();
                while let Some(&nc) = chars.peek() {
                    if nc.is_ascii_alphanumeric() || nc == '_' {
                        name.push(nc);
                        chars.next();
                    } else {
                        break;
                    }
                }
                match map.get(&name) {
                    Some(v) => out.push_str(v),
                    None => {
                        out.push('$');
                        out.push_str(&name);
                    }
                }
            }
            _ => out.push('$'),
        }
    }
    out
}

/// Parse a Dockerfile into (INSTRUCTION, args) pairs, honoring `\` line-continuations and `#` comments.
pub fn parse_dockerfile(text: &str) -> Vec<(String, String)> {
    let (mut out, mut acc) = (Vec::new(), String::new());
    for line in text.lines() {
        let l = line.trim_end();
        let t = l.trim_start();
        if acc.is_empty() && (t.is_empty() || t.starts_with('#')) {
            continue;
        }
        if let Some(s) = l.strip_suffix('\\') {
            acc.push_str(s.trim_start());
            acc.push(' ');
            continue;
        }
        acc.push_str(t);
        if let Some((inst, args)) = acc.trim().split_once(char::is_whitespace) {
            out.push((inst.to_uppercase(), args.trim().to_string()));
        }
        acc.clear();
    }
    out
}

/// A `CMD`/`ENTRYPOINT` value: JSON-array exec form `["a","b"]` or a shell string (wrapped in sh -c).
pub fn parse_exec_form(args: &str) -> Vec<String> {
    let a = args.trim();
    if a.starts_with('[') {
        if let Ok(Value::Array(v)) = serde_json::from_str::<Value>(a) {
            return v
                .into_iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect();
        }
    }
    vec!["/bin/sh".into(), "-c".into(), a.to_string()]
}

/// Parse a `LABEL` instruction's args into key/value pairs.
/// Modern form: `LABEL k=v k2="v 2" "com.x"="ACME Inc"` (one or more `key=value` pairs, values may be
/// quoted and contain spaces). Legacy form: `LABEL key the rest is the value` (no `=`, a single pair).
pub fn parse_labels(args: &str) -> Vec<(String, String)> {
    // tokenize on whitespace, honoring single/double quotes and backslash escapes.
    let (mut toks, mut cur, mut quote, mut had) =
        (Vec::<String>::new(), String::new(), '\0', false);
    let mut chars = args.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(&n) = chars.peek() {
                    cur.push(n);
                    chars.next();
                    had = true;
                }
            }
            '"' | '\'' => {
                if quote == c {
                    quote = '\0';
                } else if quote == '\0' {
                    quote = c;
                } else {
                    cur.push(c);
                }
                had = true;
            }
            c if c.is_whitespace() && quote == '\0' => {
                if had {
                    toks.push(std::mem::take(&mut cur));
                    had = false;
                }
            }
            c => {
                cur.push(c);
                had = true;
            }
        }
    }
    if had {
        toks.push(cur);
    }

    // legacy single-pair form: no token carries an '='.
    if !toks.is_empty() && toks.iter().all(|t| !t.contains('=')) {
        let key = toks[0].clone();
        return if key.is_empty() {
            vec![]
        } else {
            vec![(key, toks[1..].join(" "))]
        };
    }
    // modern form: each token is `key=value`.
    toks.into_iter()
        .filter_map(|t| {
            t.split_once('=')
                .and_then(|(k, v)| (!k.is_empty()).then(|| (k.to_string(), v.to_string())))
        })
        .collect()
}
