use crate::model::Location;

use super::{Dependency, Package};

pub(super) fn dependency(package: &Package, dependency: &Dependency) -> Location {
    let needle = dependency.alias.replace('-', "_");
    for (index, line) in package.text.lines().enumerate() {
        let trimmed = line.trim_start();
        let key = trimmed
            .split_once('=')
            .map(|(key, _)| key.trim().trim_matches(['\'', '"']).replace('-', "_"));
        if key.as_deref() == Some(&needle) {
            return Location {
                path: package.manifest.clone(),
                line: index + 1,
                column: line.len() - trimmed.len() + 1,
                source: line.to_owned(),
            };
        }
    }
    self::package(package)
}

pub(super) fn package(package: &Package) -> Location {
    let line = package
        .text
        .lines()
        .position(|line| {
            line.trim_start().starts_with("name")
                && line
                    .split_once('=')
                    .is_some_and(|(_, value)| value.trim().trim_matches('"') == package.name)
        })
        .unwrap_or(0);
    Location {
        path: package.manifest.clone(),
        line: line + 1,
        column: 1,
        source: package
            .text
            .lines()
            .nth(line)
            .unwrap_or_default()
            .to_owned(),
    }
}
