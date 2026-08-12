use std::{collections::BTreeSet, path::Path};

use crate::{
    Result,
    model::{Finding, Location, Severity},
    rule::Rule,
    source::Workspace,
};

const FORBIDDEN: &[&str] = &["common", "core", "helper", "helpers", "misc", "shared", "util", "utils"];

/// Rejects source paths named for vague reuse rather than a precise owner.
pub struct SourcePath;

impl Rule for SourcePath {
    fn id(&self) -> &'static str {
        "catch-all-source-path"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut paths = BTreeSet::new();
        for source in workspace.source_files()? {
            if let Some(stem) = source.file_stem().and_then(|value| value.to_str())
                && forbidden(stem)
            {
                paths.insert(source.clone());
            }
            for root in workspace.paths() {
                let mut directory = source.parent();
                while let Some(path) = directory {
                    if !path.starts_with(root) {
                        break;
                    }
                    if path
                        .file_name()
                        .and_then(|value| value.to_str())
                        .is_some_and(forbidden)
                    {
                        paths.insert(path.to_owned());
                    }
                    if path == root {
                        break;
                    }
                    directory = path.parent();
                }
            }
        }
        Ok(paths.into_iter().map(|path| finding(self.id(), &path)).collect())
    }
}

fn forbidden(name: &str) -> bool {
    FORBIDDEN.contains(&name)
}

fn finding(rule: &'static str, path: &Path) -> Finding {
    let name = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let mut finding = Finding::error(
        rule,
        name,
        Location {
            path: path.to_owned(),
            line: 1,
            column: 1,
            source: String::new(),
        },
    );
    finding.message = format!(
        "source path `{}` is a catch-all name that does not identify an owner",
        path.display()
    );
    finding.help =
        "name the file or directory for the entity, capability, algorithm, or external mechanism it owns".into();
    finding
}

#[cfg(test)]
#[path = "test.rs"]
mod test;
