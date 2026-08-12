use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{LintError, Result};

pub(super) fn manifests(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut manifests = Vec::new();
    for path in paths {
        discover(path, &mut manifests)?;
    }
    manifests.sort();
    manifests.dedup();
    Ok(manifests)
}

fn discover(path: &Path, manifests: &mut Vec<PathBuf>) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| LintError::io("inspect", path, error))?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_file() {
        manifests.extend(manifest_for_file(path));
        return Ok(());
    }
    collect(path, manifests).map_err(|error| LintError::io("walk", path, error))?;
    manifests.extend(workspace_manifest(path));
    Ok(())
}

fn workspace_manifest(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .map(|directory| directory.join("Cargo.toml"))
        .find(|manifest| {
            fs::read_to_string(manifest).is_ok_and(|text| {
                toml::from_str::<toml::Value>(&text).is_ok_and(|value| value.get("workspace").is_some())
            })
        })
}

fn manifest_for_file(path: &Path) -> Option<PathBuf> {
    if path.file_name().is_some_and(|name| name == "Cargo.toml") {
        Some(path.to_owned())
    } else {
        owning_manifest(path)
    }
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
