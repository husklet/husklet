use std::{
    fs, io,
    path::{Path, PathBuf},
};

use proc_macro2::Span;
use syn::{punctuated::Punctuated, Attribute, Meta, Token};

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
        let text =
            fs::read_to_string(&path).map_err(|error| LintError::io("read", &path, error))?;
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
    pub fn excerpt(&self, span: Span) -> String {
        excerpt_at(&self.text, &self.lines, span)
    }

    /// Creates a source location from a syntax span.
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
            rust_files(path, &mut files, include_linter)
                .map_err(|error| LintError::io("walk", path, error))?;
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
        let mut sources = files
            .into_iter()
            .map(Source::load)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        mark_test_modules(&mut sources);
        Ok(Self {
            sources,
            empty_directories,
            single_file_directories,
            paths,
        })
    }

    /// Returns every parsed source.
    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    /// Iterates over non-test sources.
    pub fn production(&self) -> impl Iterator<Item = &Source> {
        self.sources.iter().filter(|source| !source.test)
    }

    /// Returns repository-owned directories with no substantive entries.
    pub fn empty_directories(&self) -> &[PathBuf] {
        &self.empty_directories
    }

    /// Returns non-conventional directories containing one substantive file.
    pub fn single_file_directories(&self) -> &[(PathBuf, PathBuf)] {
        &self.single_file_directories
    }

    /// Returns the roots explicitly requested by the lint invocation.
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
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
            return components
                .next()
                .map(snake_case)
                .unwrap_or_else(|| "root".into());
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
        Meta::List(list) if list.path.is_ident("any") => parse_cfg_items(list)
            .is_some_and(|items| !items.is_empty() && items.iter().all(meta_requires_test)),
        Meta::List(_) | Meta::NameValue(_) => false,
    }
}

fn parse_cfg_items(list: &syn::MetaList) -> Option<Punctuated<Meta, Token![,]>> {
    list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
        .ok()
}

fn rust_files(path: &Path, files: &mut Vec<PathBuf>, include_linter: bool) -> io::Result<()> {
    if (!include_linter && is_linter(path)) || is_paused_domain(path) {
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
    if matches!(name, ".git" | "target" | "vendor") || (!include_linter && name == "hl-design-lint")
    {
        return Ok(());
    }
    let mut entries = fs::read_dir(path)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        rust_files(&entry.path(), files, include_linter)?;
    }
    Ok(())
}

fn directory_shapes(
    path: &Path,
    empty: &mut Vec<PathBuf>,
    single: &mut Vec<(PathBuf, PathBuf)>,
    include_linter: bool,
) -> io::Result<()> {
    if excluded(path, include_linter)
        || fs::symlink_metadata(path)?.file_type().is_symlink()
        || path.is_file()
    {
        return Ok(());
    }

    let mut entries = fs::read_dir(path)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    let substantive = entries
        .iter()
        .filter(|entry| !placeholder(&entry.path()))
        .collect::<Vec<_>>();
    if substantive.is_empty() {
        empty.push(path.to_owned());
    } else if substantive.len() == 1
        && substantive[0].file_type()?.is_file()
        && !conventional_single_file_directory(path)
    {
        single.push((path.to_owned(), substantive[0].path()));
    }
    for entry in entries {
        if entry.file_type()?.is_dir() {
            directory_shapes(&entry.path(), empty, single, include_linter)?;
        }
    }
    Ok(())
}

fn conventional_single_file_directory(path: &Path) -> bool {
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
    if (!include_linter && is_linter(path)) || is_paused_domain(path) {
        return true;
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, ".git" | "target" | "vendor" | "lint"))
}

fn is_paused_domain(path: &Path) -> bool {
    let mut components = path.components().filter_map(|component| match component {
        std::path::Component::Normal(value) => value.to_str(),
        _ => None,
    });
    while let Some(component) = components.next() {
        if component == "src" {
            return components.next().is_some_and(|domain| domain == "engine");
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
    path.file_stem().is_some_and(|stem| stem == "tests")
        || path.components().any(|component| {
            matches!(
                component.as_os_str().to_str(),
                Some("tests" | "test_support" | "benches")
            )
        })
}

/// Marks sources that only exist behind a `#[cfg(test)]` module declaration.
///
/// A file reached solely through `#[cfg(test)] mod name;` compiles only in test configuration, so its
/// contents are test support regardless of how the file is named. Recognising the declaration keeps the
/// repository's `name_test.rs` and gated-subdirectory conventions out of the production rules, which
/// otherwise report test helpers as production design findings.
fn mark_test_modules(sources: &mut [Source]) {
    let mut files = std::collections::BTreeSet::new();
    let mut directories = std::collections::BTreeSet::new();
    for source in sources.iter() {
        let Some(root) = module_root(&source.path) else {
            continue;
        };
        let Some(directory) = source.path.parent() else {
            continue;
        };
        collect_test_modules(
            &source.syntax.items,
            &root,
            directory,
            source.test,
            &mut files,
            &mut directories,
        );
    }
    for source in sources.iter_mut() {
        source.test |= files.contains(&source.path)
            || directories
                .iter()
                .any(|directory| source.path.starts_with(directory));
    }
}

/// The directory that `mod name;` declarations inside this file resolve against.
fn module_root(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    match path.file_stem()?.to_str()? {
        "mod" | "lib" | "main" => Some(parent.to_owned()),
        stem => Some(parent.join(stem)),
    }
}

/// Collects the sources gated by `#[cfg(test)]` module declarations.
///
/// `root` resolves plain `mod name;` declarations, which live in the directory owned by the module.
/// `overrides` resolves `#[path = "..."]`, which Rust reads relative to the directory of the file
/// holding the declaration until an inline module moves it deeper.
fn collect_test_modules(
    items: &[syn::Item],
    root: &Path,
    overrides: &Path,
    test_scope: bool,
    files: &mut std::collections::BTreeSet<PathBuf>,
    directories: &mut std::collections::BTreeSet<PathBuf>,
) {
    for item in items {
        let syn::Item::Mod(module) = item else {
            continue;
        };
        let gated = test_scope || requires_test(&module.attrs);
        let name = module.ident.to_string();
        match &module.content {
            // An inline module nests further declarations one directory deeper.
            Some((_, nested)) => {
                let nested_root = root.join(&name);
                collect_test_modules(
                    nested,
                    &nested_root,
                    &nested_root,
                    gated,
                    files,
                    directories,
                );
            }
            None if gated => match module_path_attribute(&module.attrs) {
                Some(relative) => {
                    files.insert(overrides.join(relative));
                }
                None => {
                    files.insert(root.join(format!("{name}.rs")));
                    // A gated module owning a directory gates every descendant with it.
                    directories.insert(root.join(name));
                }
            },
            None => {}
        }
    }
}

/// The literal of an explicit `#[path = "..."]` module override.
fn module_path_attribute(attributes: &[Attribute]) -> Option<String> {
    attributes.iter().find_map(|attribute| {
        if !attribute.path().is_ident("path") {
            return None;
        }
        let Meta::NameValue(value) = &attribute.meta else {
            return None;
        };
        let syn::Expr::Lit(literal) = &value.value else {
            return None;
        };
        let syn::Lit::Str(text) = &literal.lit else {
            return None;
        };
        Some(text.value())
    })
}
