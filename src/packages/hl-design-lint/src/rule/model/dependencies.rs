use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

pub(super) fn local_dependencies(path: &Path) -> HashSet<(String, String)> {
    let Some(manifest) = manifest(path) else {
        return HashSet::new();
    };
    let Some(owner) = package_name(&manifest) else {
        return HashSet::new();
    };
    let Ok(text) = fs::read_to_string(&manifest) else {
        return HashSet::new();
    };
    let Ok(value) = text.parse::<toml::Value>() else {
        return HashSet::new();
    };
    let workspace = workspace_manifest(&manifest);
    let workspace_dependencies = workspace
        .as_ref()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| text.parse::<toml::Value>().ok())
        .and_then(|value| value.get("workspace")?.get("dependencies").cloned());
    let mut dependencies = Vec::new();
    dependency_tables(&value, &mut dependencies);
    dependencies
        .into_iter()
        .filter_map(|(alias, specification)| {
            let specification = if specification
                .get("workspace")
                .and_then(toml::Value::as_bool)
                == Some(true)
            {
                workspace_dependencies.as_ref()?.get(alias)?
            } else {
                specification
            };
            local_package(specification, &manifest)
                .or_else(|| dependency_package(alias, specification))
                .map(|dependency| (owner.clone(), dependency))
        })
        .collect()
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

fn local_package(specification: &toml::Value, owner_manifest: &Path) -> Option<String> {
    let path = specification.get("path")?.as_str()?;
    let root = owner_manifest.parent()?.join(path);
    package_name(&root.join("Cargo.toml"))
}

fn dependency_package(alias: &str, specification: &toml::Value) -> Option<String> {
    specification.get("path")?;
    specification
        .get("package")
        .and_then(toml::Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| specification.get("path").map(|_| alias.to_owned()))
}

fn package_name(manifest: &Path) -> Option<String> {
    let text = fs::read_to_string(manifest).ok()?;
    let value = text.parse::<toml::Value>().ok()?;
    value
        .get("package")?
        .get("name")?
        .as_str()
        .map(ToOwned::to_owned)
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
                .and_then(|text| text.parse::<toml::Value>().ok())
                .is_some_and(|value| value.get("workspace").is_some())
        })
}
