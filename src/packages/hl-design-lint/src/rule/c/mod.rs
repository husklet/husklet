//! Language-aware policy for repository-owned C, Objective-C, and assembly.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{LintError, Result, source::Workspace};
mod policy;
mod structure;
mod suppression;

pub use policy::{CallPolicy, Policy};
pub use structure::Structure;

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
