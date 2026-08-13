use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use crate::{LintError, Result};

pub(super) fn local_dependencies(path: &Path) -> Result<HashSet<(String, String)>> {
    let Some(manifest) = manifest(path) else {
        return Ok(HashSet::new());
    };
    let text = fs::read_to_string(&manifest)
        .map_err(|error| LintError::configuration(format!("read {}: {error}", manifest.display())))?;
    let value = toml::from_str::<toml::Value>(&text)
        .map_err(|error| LintError::configuration(format!("decode {}: {error}", manifest.display())))?;
    let Some(owner) = value
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .map(ToOwned::to_owned)
    else {
        return Ok(HashSet::new());
    };
    let workspace = workspace_manifest(&manifest);
    let workspace_dependencies = workspace
        .as_ref()
        .map(|path| {
            let text = fs::read_to_string(path)
                .map_err(|error| LintError::configuration(format!("read {}: {error}", path.display())))?;
            let value = toml::from_str::<toml::Value>(&text)
                .map_err(|error| LintError::configuration(format!("decode {}: {error}", path.display())))?;
            Ok::<_, LintError>(
                value
                    .get("workspace")
                    .and_then(|workspace| workspace.get("dependencies"))
                    .cloned(),
            )
        })
        .transpose()?
        .flatten();
    let mut dependencies = Vec::new();
    dependency_tables(&value, &mut dependencies);
    let mut resolved = HashSet::new();
    for (alias, specification) in dependencies {
        let specification = if specification.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
            workspace_dependencies
                .as_ref()
                .and_then(|dependencies| dependencies.get(alias))
                .ok_or_else(|| {
                    LintError::configuration(format!(
                        "{} inherits dependency {alias:?}, but it is absent from [workspace.dependencies]",
                        manifest.display()
                    ))
                })?
        } else {
            specification
        };
        if let Some(dependency) =
            local_package(specification, &manifest)?.or_else(|| dependency_package(alias, specification))
        {
            resolved.insert((owner.clone(), dependency));
        }
    }
    Ok(resolved)
}

fn dependency_tables<'a>(value: &'a toml::Value, output: &mut Vec<(&'a str, &'a toml::Value)>) {
    for name in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = value.get(name).and_then(toml::Value::as_table) {
            output.extend(table.iter().map(|(name, value)| (name.as_str(), value)));
        }
    }
    if let Some(targets) = value.get("target").and_then(toml::Value::as_table) {
        for target in targets.values() {
            dependency_tables(target, output);
        }
    }
}

fn local_package(specification: &toml::Value, owner_manifest: &Path) -> Result<Option<String>> {
    let Some(path) = specification.get("path") else {
        return Ok(None);
    };
    let path = path.as_str().ok_or_else(|| {
        LintError::configuration(format!(
            "{} contains a local dependency with a non-string path",
            owner_manifest.display()
        ))
    })?;
    let root = owner_manifest
        .parent()
        .ok_or_else(|| LintError::configuration(format!("{} has no parent directory", owner_manifest.display())))?
        .join(path);
    package_name(&root.join("Cargo.toml")).map(Some)
}

fn dependency_package(alias: &str, specification: &toml::Value) -> Option<String> {
    specification.get("path")?;
    specification
        .get("package")
        .and_then(toml::Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| specification.get("path").map(|_| alias.to_owned()))
}

fn package_name(manifest: &Path) -> Result<String> {
    let text = fs::read_to_string(manifest)
        .map_err(|error| LintError::configuration(format!("read {}: {error}", manifest.display())))?;
    let value = toml::from_str::<toml::Value>(&text)
        .map_err(|error| LintError::configuration(format!("decode {}: {error}", manifest.display())))?;
    value
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| LintError::configuration(format!("{} has no package.name", manifest.display())))
}

fn manifest(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .skip(1)
        .map(|directory| directory.join("Cargo.toml"))
        .find(|manifest| manifest.is_file())
}

fn workspace_manifest(member: &Path) -> Option<PathBuf> {
    member
        .ancestors()
        .skip(1)
        .map(|directory| directory.join("Cargo.toml"))
        .find(|manifest| {
            fs::read_to_string(manifest)
                .ok()
                .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
                .is_some_and(|value| value.get("workspace").is_some())
        })
}

#[cfg(test)]
mod tests {
    use super::local_dependencies;
    use std::fs;

    #[test]
    fn rejects_malformed_owning_manifest() {
        let root = std::env::temp_dir().join(format!("lint-model-manifest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("Cargo.toml"), "[package\nname = broken\n").unwrap();
        let error = local_dependencies(&root.join("src/lib.rs")).unwrap_err();
        assert!(error.to_string().contains("decode"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn accepts_valid_manifest_without_dependencies() {
        let root = std::env::temp_dir().join(format!("lint-model-valid-manifest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"sample\"\n").unwrap();
        assert!(local_dependencies(&root.join("src/lib.rs")).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_unresolved_workspace_dependency() {
        let root = std::env::temp_dir().join(format!("lint-model-workspace-dependency-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("member/src")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"member\"]\n[workspace.dependencies]\nother = \"1\"\n",
        )
        .unwrap();
        fs::write(
            root.join("member/Cargo.toml"),
            "[package]\nname = \"member\"\n[dependencies]\nmissing.workspace = true\n",
        )
        .unwrap();
        let error = local_dependencies(&root.join("member/src/lib.rs")).unwrap_err();
        assert!(error.to_string().contains("absent from [workspace.dependencies]"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_unreadable_local_dependency_manifest() {
        let root = std::env::temp_dir().join(format!("lint-model-local-dependency-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("member/src")).unwrap();
        fs::write(
            root.join("member/Cargo.toml"),
            "[package]\nname = \"member\"\n[dependencies]\ntarget = { path = \"../missing\" }\n",
        )
        .unwrap();
        let error = local_dependencies(&root.join("member/src/lib.rs")).unwrap_err();
        assert!(error.to_string().contains("missing/Cargo.toml"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_non_string_local_dependency_path() {
        let root = std::env::temp_dir().join(format!("lint-model-local-path-type-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("member/src")).unwrap();
        fs::write(
            root.join("member/Cargo.toml"),
            "[package]\nname = \"member\"\n[dependencies]\ntarget = { path = 42 }\n",
        )
        .unwrap();
        let error = local_dependencies(&root.join("member/src/lib.rs")).unwrap_err();
        assert!(error.to_string().contains("non-string path"));
        fs::remove_dir_all(root).unwrap();
    }
}
