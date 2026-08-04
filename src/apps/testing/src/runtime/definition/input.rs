use crate::suite::Error;
use serde::{Deserialize, Deserializer};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::runtime) struct ManifestPath(String);

impl ManifestPath {
    pub(in crate::runtime) fn name(&self) -> &str {
        &self.0
    }

    pub(in crate::runtime) fn native(&self) -> PathBuf {
        self.0.split('/').collect()
    }

    #[cfg(test)]
    fn parse(value: &str) -> Result<Self, Error> {
        validate_spelling(value)?;
        Ok(Self(value.to_owned()))
    }
}

impl<'de> Deserialize<'de> for ManifestPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        validate_spelling(&value).map_err(serde::de::Error::custom)?;
        Ok(Self(value))
    }
}

pub(super) fn validate(
    directory: &Path,
    source: &ManifestPath,
    inputs: Vec<ManifestPath>,
) -> Result<Vec<ManifestPath>, Error> {
    let root = fs::canonicalize(directory)?;
    let source_file = contained_file(&root, directory, source, "source")?;
    let mut paths = BTreeSet::new();
    let mut portable_paths = BTreeSet::from([source.name().to_lowercase()]);
    let mut files = BTreeSet::new();
    for input in inputs {
        if input == *source || !paths.insert(input.clone()) || !portable_paths.insert(input.name().to_lowercase()) {
            return Err(format!("duplicate build input {}", input.name()).into());
        }
        let file = contained_file(&root, directory, &input, "input")?;
        if file == source_file || !files.insert(file) {
            return Err(format!("duplicate build input {}", input.name()).into());
        }
    }
    // Canonical path aliases are rejected. Distinct hardlink paths remain distinct
    // manifest dependencies because portable file identity is not available here.
    Ok(paths.into_iter().collect())
}

fn validate_spelling(value: &str) -> Result<(), Error> {
    if value.is_empty() {
        return Err(format!("unsafe manifest path {value:?}").into());
    }
    if value.split('/').any(invalid_segment) {
        return Err(format!("unsafe manifest path {value:?}").into());
    }
    Ok(())
}

fn invalid_segment(segment: &str) -> bool {
    if segment.is_empty()
        || matches!(segment, "." | "..")
        || segment.ends_with(['.', ' '])
        || segment.chars().any(|character| {
            character.is_control() || matches!(character, '<' | '>' | ':' | '"' | '\\' | '|' | '?' | '*')
        })
    {
        return true;
    }
    let stem = segment.split_once('.').map_or(segment, |(stem, _)| stem);
    let stem = stem.to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) || stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

fn contained_file(root: &Path, directory: &Path, path: &ManifestPath, kind: &str) -> Result<PathBuf, Error> {
    let resolved = fs::canonicalize(directory.join(path.native()))
        .map_err(|error| format!("invalid build {kind} {}: {error}", path.name()))?;
    if !resolved.starts_with(root) || !resolved.is_file() {
        Err(format!("build {kind} is not a contained file: {}", path.name()).into())
    } else {
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::{ManifestPath, validate};
    use std::fs;

    #[test]
    fn portable_multi_segment_identity_survives_native_conversion() {
        let path = ManifestPath::parse("include/a.h").unwrap();
        assert_eq!(path.name(), "include/a.h");
        assert_eq!(path.native().components().count(), 2);
    }

    #[test]
    fn rejects_host_dependent_spellings() {
        for path in [
            "",
            "/a",
            "../a",
            "a/../b",
            "a/./b",
            "a//b",
            r"a\b",
            r"C:\a",
            "C:/a",
            "a:stream",
            "a\0b",
            "a/control\u{1f}",
            "a/<bad>",
            "a/question?",
            "a/star*",
            "a/pipe|",
            "a/quote\"",
            "a/trailing.",
            "a/trailing ",
            "CON",
            "nul.txt",
            "path/Com1.log",
            "LPT9",
            "COM¹.txt",
            "lpt²",
            "clock$",
            "conin$",
        ] {
            assert!(ManifestPath::parse(path).is_err(), "accepted {path:?}");
        }
    }

    #[test]
    fn requires_regular_contained_files_and_orders_identity() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("headers")).unwrap();
        fs::write(directory.path().join("source.c"), "source").unwrap();
        fs::write(directory.path().join("z.ld"), "linker").unwrap();
        fs::write(directory.path().join("headers/a.h"), "header").unwrap();

        let inputs = validate(
            directory.path(),
            &ManifestPath::parse("source.c").unwrap(),
            vec![
                ManifestPath::parse("z.ld").unwrap(),
                ManifestPath::parse("headers/a.h").unwrap(),
            ],
        )
        .unwrap();
        assert_eq!(inputs[0].name(), "headers/a.h");
        assert_eq!(inputs[1].name(), "z.ld");

        assert!(
            validate(
                directory.path(),
                &ManifestPath::parse("source.c").unwrap(),
                vec![ManifestPath::parse("headers").unwrap()],
            )
            .is_err()
        );
        assert!(validate(directory.path(), &ManifestPath::parse("missing.c").unwrap(), Vec::new(),).is_err());
    }

    #[test]
    fn rejects_case_insensitive_source_and_input_collisions() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("source.c"), "source").unwrap();
        fs::write(directory.path().join("A.h"), "header").unwrap();

        assert!(
            validate(
                directory.path(),
                &ManifestPath::parse("source.c").unwrap(),
                vec![ManifestPath::parse("Source.c").unwrap()],
            )
            .is_err()
        );
        assert!(
            validate(
                directory.path(),
                &ManifestPath::parse("source.c").unwrap(),
                vec![ManifestPath::parse("A.h").unwrap(), ManifestPath::parse("a.h").unwrap(),],
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_source_escape_and_canonical_aliases() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), directory.path().join("source.c")).unwrap();
        assert!(validate(directory.path(), &ManifestPath::parse("source.c").unwrap(), Vec::new()).is_err());

        fs::write(directory.path().join("real.c"), "source").unwrap();
        symlink("real.c", directory.path().join("alias.c")).unwrap();
        assert!(
            validate(
                directory.path(),
                &ManifestPath::parse("real.c").unwrap(),
                vec![ManifestPath::parse("alias.c").unwrap()],
            )
            .is_err()
        );

        fs::write(directory.path().join("header.h"), "header").unwrap();
        symlink("header.h", directory.path().join("first.h")).unwrap();
        symlink("header.h", directory.path().join("second.h")).unwrap();
        assert!(
            validate(
                directory.path(),
                &ManifestPath::parse("real.c").unwrap(),
                vec![
                    ManifestPath::parse("first.h").unwrap(),
                    ManifestPath::parse("second.h").unwrap(),
                ],
            )
            .is_err()
        );
    }
}
