//! Language-aware policy for repository-owned C, Objective-C, and assembly.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{LintError, Result, source::Workspace};
use tree_sitter::{Parser, Tree};
mod allocation;
pub mod analyzer;
mod interface;
mod policy;
mod result;
mod safety;
mod structure;
mod suppression;

pub use allocation::Allocation;
pub use interface::Interface;
pub use policy::{CallPolicy, Policy};
pub use result::ResultUse;
pub use safety::Safety;
pub use structure::Structure;

fn parse(path: &Path, source: &str) -> Result<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .map_err(|error| parse_error(path, error.to_string()))?;
    let normalized = source.replace("_Thread_local", "             ");
    let tree = parser
        .parse(&normalized, None)
        .ok_or_else(|| parse_error(path, "parser returned no syntax tree"))?;
    if let Some(node) = first_unrecoverable_error(tree.root_node(), source) {
        let point = node.start_position();
        let excerpt = node
            .utf8_text(source.as_bytes())
            .unwrap_or("<non-UTF-8 syntax>")
            .lines()
            .next()
            .unwrap_or_default();
        return Err(parse_error(
            path,
            format!(
                "source contains invalid C syntax at {}:{} ({}, {excerpt:?})",
                point.row + 1,
                point.column + 1,
                node.kind()
            ),
        ));
    }
    Ok(tree)
}

fn first_unrecoverable_error<'tree>(node: tree_sitter::Node<'tree>, source: &str) -> Option<tree_sitter::Node<'tree>> {
    if node.is_error() || node.is_missing() {
        if macro_continuation(node, source)
            || line_macro_invocation(node, source)
            || conditional_statement_directive(node, source)
            || annotation_prefix(node, source)
            || builtin_offsetof_type_argument(node, source)
            || enclosing_macro_invocation(node, source)
        {
            return None;
        }
        return Some(node);
    }
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find_map(|child| first_unrecoverable_error(child, source))
}

fn conditional_statement_directive(node: tree_sitter::Node<'_>, source: &str) -> bool {
    let row = node.start_position().row;
    let lines = source.lines().collect::<Vec<_>>();
    let window = &lines[row.saturating_sub(16)..row.min(lines.len())];
    if window.iter().any(|line| line.trim_start().starts_with("#endif"))
        && window.iter().any(|line| line.trim_start().starts_with("#else"))
        && window
            .iter()
            .any(|line| matches!(line.trim_start(), line if line.starts_with("#if ") || line.starts_with("#ifdef ") || line.starts_with("#ifndef ")))
        && window.iter().any(|line| line.trim_start().starts_with("if ("))
    {
        return true;
    }
    if node.is_error() && row > 0 && lines[row].trim_start().starts_with("else ") {
        return lines[..row]
            .iter()
            .rev()
            .take_while(|line| !line.trim_start().starts_with("if ("))
            .any(|line| line.trim_start().starts_with("#endif"));
    }
    if node.kind() != ";" || !node.is_missing() {
        return false;
    }
    if row == 0 || row > lines.len() {
        return false;
    }
    let directive_row = if row < lines.len() && lines[row].trim_start().starts_with('#') {
        row
    } else if row + 1 < lines.len() && lines[row + 1].trim_start().starts_with('#') {
        row + 1
    } else {
        return false;
    };
    let previous = lines[directive_row - 1].trim_end();
    if !previous.ends_with(')') || !previous.trim_start().starts_with("if (") {
        return false;
    }
    let mut depth = 0usize;
    let mut branch_statement = false;
    for line in &lines[directive_row..] {
        let line = line.trim();
        if line.starts_with("#if ") || line.starts_with("#ifdef ") || line.starts_with("#ifndef ") {
            depth += 1;
        } else if line.starts_with("#endif") {
            let Some(next) = depth.checked_sub(1) else {
                return false;
            };
            depth = next;
            if depth == 0 {
                return branch_statement;
            }
        } else if depth == 1 && !line.is_empty() && !line.starts_with('#') {
            branch_statement = line.ends_with(';') || line.starts_with('{');
        }
    }
    false
}

fn builtin_offsetof_type_argument(mut node: tree_sitter::Node<'_>, source: &str) -> bool {
    loop {
        if node.kind() == "call_expression"
            && node
                .child_by_field_name("function")
                .and_then(|function| function.utf8_text(source.as_bytes()).ok())
                == Some("__builtin_offsetof")
        {
            return true;
        }
        let Some(parent) = node.parent() else {
            return false;
        };
        node = parent;
    }
}

fn line_macro_invocation(node: tree_sitter::Node<'_>, source: &str) -> bool {
    let lines = source.lines().collect::<Vec<_>>();
    let row = node.start_position().row;
    (row.saturating_sub(1)..=row.saturating_add(1).min(lines.len().saturating_sub(1)))
        .any(|row| defined_macro_invocation(lines[row].trim(), source))
}

fn annotation_prefix(node: tree_sitter::Node<'_>, source: &str) -> bool {
    let point = node.start_position();
    let Some(line) = source.lines().nth(point.row) else {
        return false;
    };
    let Some(prefix) = line.get(..point.column).map(str::trim) else {
        return false;
    };
    let annotation = prefix.trim_end_matches(|character: char| character.is_whitespace());
    !annotation.is_empty()
        && annotation
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_uppercase() || byte.is_ascii_digit())
        && annotation.as_bytes()[0].is_ascii_uppercase()
        && line.get(point.column..).is_some_and(|tail| tail.contains('('))
        && node
            .parent()
            .is_some_and(|parent| matches!(parent.kind(), "function_definition" | "declaration"))
}

fn macro_continuation(node: tree_sitter::Node<'_>, source: &str) -> bool {
    let lines = source.lines().collect::<Vec<_>>();
    let mut row = node.start_position().row;
    let mut first = true;
    loop {
        let line = lines[row].trim_end();
        if line.trim_start().starts_with("#define ") {
            return true;
        }
        if row == 0 {
            return false;
        }
        if !first && !line.ends_with('\\') {
            return false;
        }
        first = false;
        row -= 1;
    }
}

fn enclosing_macro_invocation(mut node: tree_sitter::Node<'_>, source: &str) -> bool {
    loop {
        if node.kind().starts_with("preproc_") {
            return true;
        }
        if recoverable_macro_invocation(node, source) {
            return true;
        }
        let Some(parent) = node.parent() else {
            return false;
        };
        node = parent;
    }
}

fn recoverable_macro_invocation(node: tree_sitter::Node<'_>, source: &str) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let context = parent.kind();
    if context != "translation_unit" && !(context == "compound_statement" && node.kind() == "expression_statement") {
        return false;
    }
    let Ok(text) = node.utf8_text(source.as_bytes()) else {
        return false;
    };
    let mut offset = 0;
    let mut invocation = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            offset += line.len();
            continue;
        }
        if defined_macro_invocation(trimmed, source) {
            invocation = true;
            offset += line.len();
            continue;
        }
        break;
    }
    invocation && (offset == text.len() || parses_without_recovery(&text[offset..]))
}

fn parses_without_recovery(source: &str) -> bool {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_c::LANGUAGE.into()).is_ok()
        && parser
            .parse(source, None)
            .is_some_and(|tree| !tree.root_node().has_error())
}

fn defined_macro_invocation(text: &str, source: &str) -> bool {
    let text = text.trim().trim_end_matches(';').trim_end();
    let Some(open) = text.find('(') else {
        return false;
    };
    let name = text[..open].trim();
    if name.is_empty()
        || !name.bytes().all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
        || name.as_bytes()[0].is_ascii_digit()
        || !balanced_parentheses(&text[open..])
    {
        return false;
    }
    source.lines().any(|line| {
        let line = line.trim_start();
        line.strip_prefix("#define")
            .is_some_and(|definition| definition.trim_start().starts_with(&format!("{name}(")))
    })
}

fn balanced_parentheses(text: &str) -> bool {
    let mut depth = 0usize;
    let length = text.len();
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                let Some(next) = depth.checked_sub(1) else {
                    return false;
                };
                depth = next;
            }
            _ => {}
        }
        if depth == 0 && index + 1 != length {
            // A matching close before the end means trailing syntax was recovered too.
            return false;
        }
    }
    depth == 0
}

fn parse_error(path: &Path, message: impl Into<String>) -> LintError {
    LintError::io(
        "parse",
        path,
        std::io::Error::new(std::io::ErrorKind::InvalidData, message.into()),
    )
}

fn source_files(workspace: &Workspace) -> Result<Vec<PathBuf>> {
    fn walk(path: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
        let metadata = fs::symlink_metadata(path).map_err(|error| LintError::io("inspect", path, error))?;
        if metadata.file_type().is_symlink() {
            return Ok(());
        }
        if metadata.is_file() {
            if path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|extension| matches!(extension, "c" | "h" | "m" | "mm"))
            {
                output.push(path.to_owned());
            }
            return Ok(());
        }
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| matches!(name, ".git" | "target" | "vendor"))
        {
            return Ok(());
        }
        let mut entries = fs::read_dir(path)
            .map_err(|error| LintError::io("read source directory", path, error))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| LintError::io("read source directory", path, error))?;
        entries.sort_by_key(std::fs::DirEntry::path);
        for entry in entries {
            walk(&entry.path(), output)?;
        }
        Ok(())
    }

    let mut output = Vec::new();
    for root in workspace.paths() {
        walk(root, &mut output)?;
    }
    output.sort();
    output.dedup();
    Ok(output)
}

#[cfg(test)]
mod test {
    use super::parse;
    use std::path::Path;

    #[test]
    fn parser_accepts_valid_c() {
        assert!(parse(Path::new("valid.c"), "int answer(void) { return 42; }").is_ok());
    }

    #[test]
    fn parser_rejects_recovered_syntax_errors() {
        let error = parse(Path::new("invalid.c"), "int answer(void) { return ; trailing }").unwrap_err();
        assert!(error.to_string().contains("invalid C syntax"));
    }

    #[test]
    fn parser_accepts_defined_top_level_macro_with_an_empty_argument() {
        let source = "#define MAKE(name, ty, suffix) ty name(ty value) { return value; }\n\
                      MAKE(identity, int, )\n";
        parse(Path::new("generated.c"), source).unwrap();
    }

    #[test]
    fn parser_rejects_undeclared_top_level_recovery() {
        assert!(parse(Path::new("invalid.c"), "UNKNOWN(identity, int, )\n").is_err());
    }

    #[test]
    fn parser_accepts_declared_function_scope_macro_invocation() {
        let source = "#define EACH_FIELD(X) X(first) X(second)\n\
                      int valid(int first, int second) {\n\
                          EACH_FIELD(VALIDATE)\n\
                          return 1;\n\
                      }\n";
        assert!(parse(Path::new("function-macro.c"), source).is_ok());
    }

    #[test]
    fn parser_rejects_undeclared_function_scope_recovery() {
        let source = "int invalid(void) {\n UNKNOWN_MACRO(value)\n return 0;\n}\n";
        assert!(parse(Path::new("invalid.c"), source).is_err());
    }

    #[test]
    fn parser_accepts_error_node_covering_a_multiline_definition() {
        let source = "#define DISPATCH(context) \\\n+                          if ((context)->ready) { \\\n+                              continue; \\\n+                          } else { \\\n+                              break; \\\n+                          }\n";
        assert!(parse(Path::new("dispatch.h"), source).is_ok());
    }

    #[test]
    fn parser_accepts_error_on_final_uncontinued_macro_line() {
        let source = "#define BODY(value) \\\n+                          do { \\\n+                              value++; \\\n+                          } while (0)\n";
        assert!(parse(Path::new("dispatch.h"), source).is_ok());
    }

    #[test]
    fn parser_accepts_c11_thread_local_storage() {
        let source = "typedef struct Options Options;\nstatic _Thread_local Options *current;\n";
        assert!(parse(Path::new("storage.c"), source).is_ok());
    }

    #[test]
    fn parser_accepts_builtin_offsetof_with_a_struct_type() {
        let source = "struct cpu { unsigned long sigmask; };\n\
                      int offset(void) { return (int)__builtin_offsetof(struct cpu, sigmask); }\n";
        assert!(parse(Path::new("offset.c"), source).is_ok());
    }

    #[test]
    fn parser_accepts_uppercase_function_annotation() {
        let source = "PUBLIC_API int answer(void) { return 42; }\n";
        assert!(parse(Path::new("annotated.c"), source).is_ok());
    }

    #[test]
    fn parser_rejects_arbitrary_tokens_before_a_function() {
        let source = "not_an_annotation int answer(void) { return 42; }\n";
        assert!(parse(Path::new("invalid.c"), source).is_err());
    }

    #[test]
    fn parser_accepts_conditional_single_statement_after_if() {
        let source = "int open_file(int access) {\n\
                          int flags;\n\
                          if (access)\n\
                      #ifdef FEATURE_FLAG\n\
                              flags = 1;\n\
                      #else\n\
                              flags = 2;\n\
                      #endif\n\
                          return flags;\n\
                      }\n";
        assert!(parse(Path::new("conditional.c"), source).is_ok());
    }

    #[test]
    fn parser_rejects_unclosed_conditional_after_if() {
        let source = "int invalid(int access) {\n\
                          if (access)\n\
                      #ifdef FEATURE_FLAG\n\
                              return 1;\n\
                      }\n";
        assert!(parse(Path::new("invalid.c"), source).is_err());
    }
}
