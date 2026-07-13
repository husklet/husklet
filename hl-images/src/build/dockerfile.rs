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

/// The line-continuation character, honoring a leading `# escape=` parser directive. Docker's directive
/// selects backslash (default) or backtick as the escape/continuation char; the directive must appear in
/// the leading comment block (before the first instruction). With `# escape=\`` a trailing backtick
/// continues a line and a backslash is literal.
fn escape_char(text: &str) -> char {
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let Some(comment) = t.strip_prefix('#') else {
            break; // first instruction: directives may only precede it
        };
        // a parser directive is `# <name>=<value>`; we only care about `escape`.
        if let Some((name, value)) = comment.split_once('=') {
            if name.trim().eq_ignore_ascii_case("escape") {
                return if value.trim().starts_with('`') { '`' } else { '\\' };
            }
        }
    }
    '\\'
}

/// Parse a Dockerfile into (INSTRUCTION, args) pairs, honoring line-continuations (`\` by default, or the
/// `# escape=` parser directive's char) and `#` comments.
pub fn parse_dockerfile(text: &str) -> Vec<(String, String)> {
    let esc = escape_char(text);
    let (mut out, mut acc) = (Vec::new(), String::new());
    for line in text.lines() {
        let l = line.trim_end();
        let t = l.trim_start();
        if acc.is_empty() && (t.is_empty() || t.starts_with('#')) {
            continue;
        }
        if let Some(s) = l.strip_suffix(esc) {
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
///
/// A JSON array with a NON-STRING element (`["echo", 123]`) is invalid exec form: rather than silently
/// truncating it to just the string elements (the old behavior, which dropped the `123` and could turn a
/// meaningful command into a partial one), it is rejected by [`parse_exec_form_checked`] and this lenient
/// wrapper falls back to treating the raw text as a shell command.
pub fn parse_exec_form(args: &str) -> Vec<String> {
    match parse_exec_form_checked(args) {
        Ok(v) => v,
        Err(_) => vec!["/bin/sh".into(), "-c".into(), args.trim().to_string()],
    }
}

/// Strict [`parse_exec_form`]: an exec-form JSON array containing a non-string element is an ERROR
/// (matching docker/BuildKit, which reject `CMD ["echo", 123]`) instead of being silently truncated. A
/// value that is not a JSON array is shell form and always parses (`Ok`).
pub fn parse_exec_form_checked(args: &str) -> Result<Vec<String>, String> {
    let a = args.trim();
    if a.starts_with('[') {
        if let Ok(Value::Array(v)) = serde_json::from_str::<Value>(a) {
            let mut out = Vec::with_capacity(v.len());
            for x in v {
                match x {
                    Value::String(s) => out.push(s),
                    other => {
                        return Err(format!(
                            "exec form must be an array of strings; got non-string element {other}"
                        ))
                    }
                }
            }
            return Ok(out);
        }
        // a `[`-prefixed value that isn't valid JSON (`[not json]`) is treated as shell form, unchanged.
    }
    Ok(vec!["/bin/sh".into(), "-c".into(), a.to_string()])
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

    // Legacy single-pair form vs. modern `key=value…` form is decided on the FIRST word ALONE, exactly
    // as BuildKit's parseNameVal does (`if !strings.Contains(words[0], "=")` → legacy, value = rest of
    // line). A legacy value may itself contain `=` (`ENV GREETING hello=world` → GREETING="hello=world"),
    // so we must NOT gate on whether *any* token carries `=` — that misclassifies such lines as modern
    // and silently drops the real variable name (the same class as the earlier multi-pair ENV bug).
    if !toks.is_empty() && !toks[0].contains('=') {
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

/// Parse an `ENV` instruction's arguments into `(key, value)` pairs. `ENV` shares `LABEL`'s exact
/// Dockerfile grammar — the `ENV key=value [key2=value2 …]` form (quotes preserve spaces, multiple
/// pairs per line) and the legacy `ENV key value…` form (one variable, value = rest of line) — so it
/// delegates to [`parse_labels`]. This fixes the old inline handler that split on the first `=`, kept
/// only the first whitespace-token of the value (dropping quoted spaces) and discarded extra pairs.
pub fn parse_env(args: &str) -> Vec<(String, String)> {
    parse_labels(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn parse_dockerfile_continuations_and_comments() {
        let df = "\
# a comment
FROM ubuntu:22.04

RUN echo hi \\
    && echo bye
CMD [\"a\",\"b\"]
";
        assert_eq!(
            parse_dockerfile(df),
            vec![
                ("FROM".to_string(), "ubuntu:22.04".to_string()),
                // continuation joins with the retained trailing space + the added separator space
                ("RUN".to_string(), "echo hi  && echo bye".to_string()),
                ("CMD".to_string(), "[\"a\",\"b\"]".to_string()),
            ]
        );
    }

    // Finding A: a `# escape=\`` parser directive makes a trailing BACKTICK continue the line (and a
    // backslash literal). The backtick-continued RUN must parse as ONE instruction, not split.
    #[test]
    fn parse_dockerfile_honors_backtick_escape_directive() {
        let df = "# escape=`\nFROM ubuntu\nRUN echo hi `\n    && echo bye\n";
        assert_eq!(
            parse_dockerfile(df),
            vec![
                ("FROM".to_string(), "ubuntu".to_string()),
                ("RUN".to_string(), "echo hi  && echo bye".to_string()),
            ]
        );
        // with `# escape=\``, a trailing BACKSLASH is literal (no longer a continuation): the line stands
        // alone and the backslash is preserved.
        let df2 = "# escape=`\nRUN echo one\\\nRUN echo two\n";
        assert_eq!(
            parse_dockerfile(df2),
            vec![
                ("RUN".to_string(), "echo one\\".to_string()),
                ("RUN".to_string(), "echo two".to_string()),
            ]
        );
    }

    #[test]
    fn parse_dockerfile_default_escape_is_backslash() {
        // no directive -> backslash still continues (unchanged default behavior).
        let df = "RUN a \\\n  b\n";
        assert_eq!(parse_dockerfile(df), vec![("RUN".to_string(), "a  b".to_string())]);
    }

    // Finding B: an exec-form array with a non-string element is INVALID and must be rejected (error),
    // not silently truncated to the string elements.
    #[test]
    fn parse_exec_form_checked_rejects_non_string_element() {
        assert!(
            parse_exec_form_checked("[\"echo\", 123]").is_err(),
            "a non-string exec-form element must be an error"
        );
        // a valid all-string array is Ok and verbatim.
        assert_eq!(
            parse_exec_form_checked("[\"echo\", \"hi\"]").unwrap(),
            vec!["echo".to_string(), "hi".to_string()]
        );
        // shell form is always Ok.
        assert_eq!(
            parse_exec_form_checked("echo hi").unwrap(),
            vec!["/bin/sh".to_string(), "-c".to_string(), "echo hi".to_string()]
        );
        // the lenient wrapper no longer silently truncates: `["echo", 123]` is NOT `["echo"]`.
        assert_ne!(parse_exec_form("[\"echo\", 123]"), vec!["echo".to_string()]);
    }

    #[test]
    fn parse_exec_form_json_vs_shell() {
        // JSON exec form -> the argv verbatim
        assert_eq!(
            parse_exec_form("[\"echo\", \"hi\"]"),
            vec!["echo".to_string(), "hi".to_string()]
        );
        // shell form -> wrapped in /bin/sh -c
        assert_eq!(
            parse_exec_form("echo hi"),
            vec!["/bin/sh".to_string(), "-c".to_string(), "echo hi".to_string()]
        );
        // malformed JSON array falls back to shell form
        assert_eq!(
            parse_exec_form("[not json]"),
            vec!["/bin/sh".to_string(), "-c".to_string(), "[not json]".to_string()]
        );
    }

    #[test]
    fn parse_labels_modern_and_legacy() {
        // modern: key=value pairs, quoted values may contain spaces
        assert_eq!(
            parse_labels("k=v k2=\"v 2\" \"com.x\"=\"ACME Inc\""),
            vec![
                ("k".to_string(), "v".to_string()),
                ("k2".to_string(), "v 2".to_string()),
                ("com.x".to_string(), "ACME Inc".to_string()),
            ]
        );
        // legacy: no `=` anywhere -> a single pair, rest-is-value
        assert_eq!(
            parse_labels("maintainer John Doe"),
            vec![("maintainer".to_string(), "John Doe".to_string())]
        );
    }

    #[test]
    fn parse_env_multi_pair_quotes_and_legacy() {
        // Regression: the old inline ENV handler split on the first '=', kept only the first
        // whitespace-token of the value, and dropped extra pairs. Correct Dockerfile ENV behavior:
        // multiple key=value pairs on one line.
        assert_eq!(
            parse_env("A=1 B=2"),
            vec![
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "2".to_string()),
            ]
        );
        // a quoted value preserves its spaces (was truncated to "hello" before).
        assert_eq!(
            parse_env("FOO=\"hello world\""),
            vec![("FOO".to_string(), "hello world".to_string())]
        );
        // legacy `ENV key value…` form: one variable, value = rest of the line (spaces kept).
        assert_eq!(
            parse_env("FOO bar baz"),
            vec![("FOO".to_string(), "bar baz".to_string())]
        );
    }

    #[test]
    fn parse_env_legacy_value_may_contain_equals() {
        // Regression: the form is chosen on the FIRST word alone (BuildKit parseNameVal). A legacy
        // value that itself contains '=' must NOT flip the line to the modern multi-pair form, which
        // would silently drop the real variable name and inject a garbage `flag=1` var.
        assert_eq!(
            parse_env("GREETING hello=world"),
            vec![("GREETING".to_string(), "hello=world".to_string())]
        );
        assert_eq!(
            parse_env("DEBUG_OPTS --flag=1 --other"),
            vec![("DEBUG_OPTS".to_string(), "--flag=1 --other".to_string())]
        );
        // same first-word rule for LABEL.
        assert_eq!(
            parse_labels("description This is version=2 of the app"),
            vec![(
                "description".to_string(),
                "This is version=2 of the app".to_string()
            )]
        );
        // first word DOES carry '=' -> modern form, every token is a pair (unchanged behavior).
        assert_eq!(
            parse_env("A=1 B=2 C=x=y"),
            vec![
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "2".to_string()),
                ("C".to_string(), "x=y".to_string()),
            ]
        );
    }

    #[test]
    fn substitute_args_forms() {
        let m = args(&[("FOO", "bar")]);
        // both $NAME and ${NAME}
        assert_eq!(substitute_args("$FOO/x", &m), "bar/x");
        assert_eq!(substitute_args("${FOO}x", &m), "barx");
        // undefined ${NAME} expands to empty; undefined $NAME is left literal
        assert_eq!(substitute_args("a${UNDEF}b", &m), "ab");
        assert_eq!(substitute_args("a $UNDEF b", &m), "a $UNDEF b");
        // combined
        assert_eq!(
            substitute_args("$FOO ${FOO} $UNDEF ${UNDEF}!", &m),
            "bar bar $UNDEF !"
        );
        // empty map / no `$` -> unchanged
        assert_eq!(substitute_args("$FOO", &HashMap::new()), "$FOO");
        assert_eq!(substitute_args("plain", &m), "plain");
    }
}
