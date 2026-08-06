use std::{
    fs, io,
    path::{Path, PathBuf},
};

use proc_macro2::Span;
use syn::{Attribute, Meta, Token, punctuated::Punctuated};

use crate::{LintError, Result};

/// One parsed Rust source with repository ownership metadata.
pub struct Source {
    /// Source path.
    pub path: PathBuf,
    /// Complete source text.
    pub text: String,
    /// Parsed Rust syntax tree.
    pub syntax: syn::File,
    /// Owning Cargo package.
    pub package: String,
    /// Repository domain below `src/`.
    pub domain: String,
    /// Whether the path belongs to test support.
    pub test: bool,
    lines: Vec<usize>,
}

impl Source {
    fn load(path: PathBuf) -> Result<Self> {
        let text = fs::read_to_string(&path).map_err(|error| LintError::io("read", &path, error))?;
        let syntax = syn::parse_file(&text).map_err(|source| LintError::Parse {
            path: path.clone(),
            source,
        })?;
        let lines = line_offsets(&text);
        Ok(Self {
            package: package(&path).unwrap_or_else(|| "unknown-package".to_owned()),
            domain: domain(&path),
            test: is_test(&path),
            path,
            text,
            syntax,
            lines,
        })
    }

    /// Extracts text covered by a syntax span.
    #[must_use]
    pub fn excerpt(&self, span: Span) -> String {
        excerpt_at(&self.text, &self.lines, span)
    }

    /// Creates a source location from a syntax span.
    #[must_use]
    pub fn location(&self, span: Span) -> crate::model::Location {
        let start = span.start();
        crate::model::Location {
            path: self.path.clone(),
            line: start.line,
            column: start.column + 1,
            source: self.excerpt(span),
        }
    }
}

/// Parsed Rust sources discovered from requested paths.
pub struct Workspace {
    sources: Vec<Source>,
    empty_directories: Vec<PathBuf>,
    single_file_directories: Vec<(PathBuf, PathBuf)>,
    paths: Vec<PathBuf>,
}

impl Workspace {
    /// Discovers, reads, and parses Rust sources once.
    pub fn load(paths: impl IntoIterator<Item = PathBuf>) -> Result<Self> {
        let paths = paths.into_iter().collect::<Vec<_>>();
        let mut files = Vec::new();
        let mut empty_directories = Vec::new();
        let mut single_file_directories = Vec::new();
        for path in &paths {
            let include_linter = explicit_linter(path);
            rust_files(path, &mut files, include_linter).map_err(|error| LintError::io("walk", path, error))?;
            directory_shapes(
                path,
                &mut empty_directories,
                &mut single_file_directories,
                include_linter,
            )
            .map_err(|error| LintError::io("walk", path, error))?;
        }
        files.sort();
        files.dedup();
        empty_directories.sort();
        empty_directories.dedup();
        single_file_directories.sort();
        single_file_directories.dedup();
        Ok(Self {
            sources: files
                .into_iter()
                .map(Source::load)
                .collect::<std::result::Result<_, _>>()?,
            empty_directories,
            single_file_directories,
            paths,
        })
    }

    /// Returns every parsed source.
    #[must_use]
    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    /// Iterates over non-test sources.
    pub fn production(&self) -> impl Iterator<Item = &Source> {
        self.sources.iter().filter(|source| !source.test)
    }

    /// Returns repository-owned directories with no substantive entries.
    #[must_use]
    pub fn empty_directories(&self) -> &[PathBuf] {
        &self.empty_directories
    }

    /// Returns non-conventional directories containing one substantive file.
    #[must_use]
    pub fn single_file_directories(&self) -> &[(PathBuf, PathBuf)] {
        &self.single_file_directories
    }

    /// Returns the roots explicitly requested by the lint invocation.
    #[must_use]
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Returns repository-owned Rust, C, and C-header files below a source or test root.
    pub(crate) fn source_files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for path in &self.paths {
            source_files(path, &mut files)?;
        }
        files.sort();
        files.dedup();
        Ok(files)
    }
}

fn line_offsets(source: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    offsets.extend(
        source
            .bytes()
            .enumerate()
            .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
    );
    offsets
}

fn excerpt_at(source: &str, offsets: &[usize], span: Span) -> String {
    let start = span.start();
    let end = span.end();
    let Some(start_line) = offsets.get(start.line.saturating_sub(1)) else {
        return String::new();
    };
    let Some(end_line) = offsets.get(end.line.saturating_sub(1)) else {
        return String::new();
    };
    let start = start_line.saturating_add(start.column);
    let end = end_line.saturating_add(end.column).min(source.len());
    source.get(start..end).unwrap_or_default().to_owned()
}

pub fn package(path: &Path) -> Option<String> {
    for directory in path.ancestors().skip(1) {
        let Ok(manifest) = fs::read_to_string(directory.join("Cargo.toml")) else {
            continue;
        };
        let mut in_package = false;
        for line in manifest.lines().map(str::trim) {
            if line.starts_with('[') {
                in_package = line == "[package]";
            } else if in_package && line.starts_with("name") {
                return line
                    .split_once('=')
                    .map(|(_, name)| name.trim().trim_matches('"').to_owned());
            }
        }
    }
    None
}

pub fn domain(path: &Path) -> String {
    let mut components = path.components().filter_map(|component| match component {
        std::path::Component::Normal(value) => value.to_str(),
        _ => None,
    });
    while let Some(component) = components.next() {
        if component == "src" {
            return components.next().map_or_else(|| "root".into(), snake_case);
        }
    }
    "root".to_owned()
}

pub fn snake_case(value: &str) -> String {
    let mut output = String::new();
    let mut separator = false;
    for character in value.trim_start_matches("r#").chars() {
        if character.is_ascii_alphanumeric() {
            if (character.is_ascii_uppercase()
                && output
                    .chars()
                    .last()
                    .is_some_and(|character| character.is_ascii_lowercase()))
                || (separator && !output.is_empty())
            {
                output.push('_');
            }
            output.push(character.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    output.trim_matches('_').to_owned()
}

pub fn requires_test(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<Meta>()
                .is_ok_and(|meta| meta_requires_test(&meta))
    })
}

fn meta_requires_test(meta: &Meta) -> bool {
    match meta {
        Meta::Path(path) => path.is_ident("test"),
        Meta::List(list) if list.path.is_ident("all") => {
            parse_cfg_items(list).is_some_and(|items| items.iter().any(meta_requires_test))
        }
        Meta::List(list) if list.path.is_ident("any") => {
            parse_cfg_items(list).is_some_and(|items| !items.is_empty() && items.iter().all(meta_requires_test))
        }
        Meta::List(_) | Meta::NameValue(_) => false,
    }
}

fn parse_cfg_items(list: &syn::MetaList) -> Option<Punctuated<Meta, Token![,]>> {
    list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
        .ok()
}

fn rust_files(path: &Path, files: &mut Vec<PathBuf>, include_linter: bool) -> io::Result<()> {
    if (!include_linter && is_linter(path)) || is_foreign_source(path) || is_external_corpus(path) {
        return Ok(());
    }
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Ok(());
    }
    if path.is_file() {
        if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path.to_owned());
        }
        return Ok(());
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    if matches!(name, ".git" | "target" | "vendor") || (!include_linter && name == "hl-design-lint") {
        return Ok(());
    }
    let mut entries = fs::read_dir(path)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::path);
    for entry in entries {
        rust_files(&entry.path(), files, include_linter)?;
    }
    Ok(())
}

fn source_files(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| LintError::io("inspect", path, error))?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_file() {
        let extension = path.extension().and_then(|value| value.to_str());
        if matches!(extension, Some("rs" | "c" | "h")) && below_architecture_root(path) {
            files.push(path.to_owned());
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
        source_files(&entry.path(), files)?;
    }
    Ok(())
}

fn below_architecture_root(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component.as_os_str().to_str(), Some("src" | "tests")))
}

fn directory_shapes(
    path: &Path,
    empty: &mut Vec<PathBuf>,
    single: &mut Vec<(PathBuf, PathBuf)>,
    include_linter: bool,
) -> io::Result<()> {
    if excluded(path, include_linter) || fs::symlink_metadata(path)?.file_type().is_symlink() || path.is_file() {
        return Ok(());
    }

    let mut entries = fs::read_dir(path)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::path);
    let substantive = entries
        .iter()
        .filter(|entry| !placeholder(&entry.path()))
        .collect::<Vec<_>>();
    if substantive.is_empty() {
        empty.push(path.to_owned());
    } else if substantive.len() == 1 && substantive[0].file_type()?.is_file() && !conventional_single_directory(path) {
        single.push((path.to_owned(), substantive[0].path()));
    }
    for entry in entries {
        if entry.file_type()?.is_dir() {
            directory_shapes(&entry.path(), empty, single, include_linter)?;
        }
    }
    Ok(())
}

fn conventional_single_directory(path: &Path) -> bool {
    let name = path.file_name().and_then(|name| name.to_str());
    let artifact_boundary = path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some("references" | "registry" | "golden")
        )
    });
    matches!(
        name,
        Some("tests" | "benches" | "examples" | "migrations" | "bin" | ".cargo")
    ) || artifact_boundary
        || (name == Some("src") && path.join("../Cargo.toml").is_file())
        || (path.join("tests.rs").is_file()
            && path
                .parent()
                .zip(name)
                .is_some_and(|(parent, name)| parent.join(format!("{name}.rs")).is_file()))
}

fn placeholder(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, ".gitkeep" | ".keep" | ".DS_Store"))
}

fn excluded(path: &Path, include_linter: bool) -> bool {
    if (!include_linter && is_linter(path)) || is_foreign_source(path) || is_external_corpus(path) {
        return true;
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, ".git" | "target" | "vendor" | "lint"))
}

fn is_external_corpus(path: &Path) -> bool {
    path.ancestors().any(|ancestor| {
        ancestor.join(".hl-external-corpus").is_file() || ancestor.join(".hl-generated-artifacts").is_file()
    })
}

fn is_foreign_source(path: &Path) -> bool {
    let mut components = path.components().filter_map(|component| match component {
        std::path::Component::Normal(value) => value.to_str(),
        _ => None,
    });
    while let Some(component) = components.next() {
        if component == "src" {
            return components
                .next()
                .is_some_and(|layer| matches!(layer, "native" | "schema"));
        }
    }
    false
}

fn is_linter(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| name == "hl-design-lint")
    })
}

fn explicit_linter(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "hl-design-lint")
        && path.join("Cargo.toml").is_file()
}

fn is_test(path: &Path) -> bool {
    path.file_stem().is_some_and(|stem| {
        stem == "test"
            || stem == "tests"
            || stem
                .to_str()
                .is_some_and(|name| name.ends_with("_test") || name.ends_with("_tests"))
    }) || path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some("tests" | "test_support" | "benches")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::Workspace;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn external_corpus_ignored() {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("hl-lint-corpus-{nonce}"));
        let imported = root.join("oracle/only");
        fs::create_dir_all(&imported).unwrap();
        fs::write(root.join("oracle/.hl-external-corpus"), b"external\n").unwrap();
        fs::write(imported.join("fixture.c"), b"int main(void) { return 0; }\n").unwrap();
        let workspace = Workspace::load([root.clone()]).unwrap();
        assert!(workspace.single_file_directories().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn generated_artifacts_ignored() {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("hl-lint-artifacts-{nonce}"));
        let generated = root.join("work/empty");
        fs::create_dir_all(&generated).unwrap();
        fs::write(root.join("work/.hl-generated-artifacts"), b"generated\n").unwrap();
        let workspace = Workspace::load([root.clone()]).unwrap();
        assert!(workspace.empty_directories().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
