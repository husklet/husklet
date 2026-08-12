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
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| parse_error(path, "parser returned no syntax tree"))?;
    if tree.root_node().has_error() {
        return Err(parse_error(path, "source contains invalid C syntax"));
    }
    Ok(tree)
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
}
