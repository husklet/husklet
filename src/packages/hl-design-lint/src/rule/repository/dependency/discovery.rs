use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{LintError, Result};

pub(super) fn manifests(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut manifests = Vec::new();
    for path in paths {
        let metadata = fs::symlink_metadata(path).map_err(|error| LintError::io("inspect", path, error))?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_file() {
            if path.file_name().is_some_and(|name| name == "Cargo.toml") {
                manifests.push(path.clone());
            } else if let Some(manifest) = owning_manifest(path) {
                manifests.push(manifest);
            }
        } else {
            collect(path, &mut manifests).map_err(|error| LintError::io("walk", path, error))?;
            if let Some(workspace) = path
                .ancestors()
                .map(|directory| directory.join("Cargo.toml"))
                .find(|manifest| {
                    fs::read_to_string(manifest).is_ok_and(|text| {
                        toml::from_str::<toml::Value>(&text).is_ok_and(|value| value.get("workspace").is_some())
                    })
                })
            {
                manifests.push(workspace);
            }
        }
    }
    manifests.sort();
    manifests.dedup();
    Ok(manifests)
}

fn collect(path: &Path, output: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, ".git" | "target" | "vendor" | "lint"))
    {
        return Ok(());
    }
    if path.join("Cargo.toml").is_file() {
        output.push(path.join("Cargo.toml"));
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !entry.file_type()?.is_symlink() {
            collect(&entry.path(), output)?;
        }
    }
    Ok(())
}

pub(super) fn owning_manifest(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .skip(1)
        .map(|directory| directory.join("Cargo.toml"))
        .find(|manifest| manifest.is_file())
}
