use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use proc_macro2::Span;
use syn::{ImplItemFn, ItemEnum, ItemFn, ItemStruct, ItemTrait, TraitItemFn, spanned::Spanned, visit::Visit};

use crate::{
    Result,
    model::{Finding, Location, Review, Severity},
    rule::Rule,
    source::{Source, Workspace},
};

#[cfg(test)]
#[path = "test.rs"]
mod tests;

/// Rejects source filenames that encode more than two concepts.
pub struct FileName;

impl Rule for FileName {
    fn id(&self) -> &'static str {
        "file-name-density"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        Ok(workspace
            .sources()
            .iter()
            .filter_map(|source| {
                let stem = source.path.file_stem()?.to_str()?;
                if conventional(stem) {
                    return None;
                }
                if numbered_fragment(stem) {
                    let mut finding = Finding::error(
                        self.id(),
                        stem,
                        file_location(&source.path),
                    );
                    finding.message = format!(
                        "Rust filename `{stem}.rs` uses a numbered implementation fragment"
                    );
                    finding.help = "split by a cohesive noun-owned concept; numeric fragments hide ownership and ordering instead of naming it".into();
                    return Some(finding);
                }
                if words(stem).len() <= 2 {
                    return None;
                }
                let mut finding = Finding::error(
                    self.id(),
                    stem,
                    file_location(&source.path),
                );
                finding.message = format!(
                    "Rust filename `{stem}.rs` contains more than two semantic words"
                );
                finding.help = "move the shared concept into a noun directory and name this file for its one- or two-word child concept".into();
                Some(finding)
            })
            .collect())
    }
}

/// Enforces singular companion-test filenames.
pub struct TestName;

impl Rule for TestName {
    fn id(&self) -> &'static str {
        "singular-test-file"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        Ok(workspace
            .sources()
            .iter()
            .filter_map(|source| {
                let stem = source.path.file_stem()?.to_str()?;
                (stem == "tests" || stem.ends_with("_tests")).then(|| {
                    let mut finding =
                        Finding::error(self.id(), stem, file_location(&source.path));
                    finding.message =
                        format!("test file `{stem}.rs` uses the plural `_tests.rs` convention");
                    finding.help = "prefer inline #[cfg(test)] tests; when the production file would otherwise exceed 500 lines, use singular `_test.rs`".into();
                    finding
                })
            })
            .collect())
    }
}

/// Requires a noun module when a flat prefix accumulates three files.
pub struct PrefixDirectory;

impl Rule for PrefixDirectory {
    fn id(&self) -> &'static str {
        "flat-prefix-density"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut groups: BTreeMap<(PathBuf, String), Vec<&Source>> = BTreeMap::new();
        for source in workspace.sources() {
            let Some(parent) = source.path.parent() else {
                continue;
            };
            let Some(stem) = source.path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if conventional(stem) {
                continue;
            }
            let tokens = words(stem);
            if tokens.len() < 2 {
                continue;
            }
            groups
                .entry((parent.to_owned(), tokens[0].clone()))
                .or_default()
                .push(source);
        }
        Ok(groups
            .into_iter()
            .filter(|(_, sources)| sources.len() >= 3)
            .map(|((parent, prefix), sources)| {
                let names = sources
                    .iter()
                    .filter_map(|source| source.path.file_name()?.to_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut finding = Finding::error(self.id(), prefix.clone(), file_location(&sources[0].path));
                finding.message = format!("{} sibling files repeat the `{prefix}` prefix: {names}", sources.len());
                finding.help = format!(
                    "create the noun module `{}/` and remove `{prefix}_` from its child filenames and declarations",
                    parent.join(&prefix).display()
                );
                let mut review = Review::error();
                review.metadata.push(("siblings".into(), sources.len().to_string()));
                review.questions = vec![
                    format!("Is `{prefix}` the precise noun owning these children?"),
                    "Do child declarations repeat the module noun?".into(),
                ];
                finding.review = Some(review);
                finding
            })
            .collect())
    }
}

/// Requires Rust module directories to be named by a noun-bearing concept.
pub struct FolderNoun;

impl Rule for FolderNoun {
    fn id(&self) -> &'static str {
        "folder-noun"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut directories = workspace
            .sources()
            .iter()
            .filter_map(|source| source.path.parent().map(Path::to_owned))
            .collect::<Vec<_>>();
        directories.sort();
        directories.dedup();
        Ok(directories
            .into_iter()
            .filter(|directory| module_directory(directory))
            .filter_map(|directory| {
                let name = directory.file_name()?.to_str()?;
                let tokens = words(name);
                let invalid = tokens.len() != 1
                    || tokens
                        .first()
                        .is_some_and(|word| action_form(word));
                invalid.then(|| {
                    let mut finding =
                        Finding::error(self.id(), name, file_location(&directory));
                    finding.message = format!(
                        "Rust module folder `{name}` must be one semantic noun word"
                    );
                    finding.help = "name the directory for the entity or domain it owns; lexical enforcement is intentionally conservative and rejects multiword names plus a narrow action-form denylist".into();
                    finding
                })
            })
            .collect())
    }
}

/// Rejects declarations whose names contain four or more semantic words.
pub struct SymbolName;

impl Rule for SymbolName {
    fn id(&self) -> &'static str {
        "symbol-name-density"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for source in workspace.sources() {
            let mut visitor = Symbols {
                rule: self.id(),
                source,
                trait_impl: false,
                findings: &mut findings,
            };
            visitor.visit_file(&source.syntax);
        }
        Ok(findings)
    }
}

/// Rejects declarations that repeat the noun already supplied by their module.
pub struct ModulePrefix;

impl Rule for ModulePrefix {
    fn id(&self) -> &'static str {
        "redundant-module-prefix"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for source in workspace.sources() {
            let Some(noun) = module_noun(&source.path) else {
                continue;
            };
            let mut visitor = Prefixes {
                rule: self.id(),
                source,
                noun,
                trait_impl: false,
                findings: &mut findings,
            };
            visitor.visit_file(&source.syntax);
        }
        Ok(findings)
    }
}

struct Symbols<'a> {
    rule: &'static str,
    source: &'a Source,
    trait_impl: bool,
    findings: &'a mut Vec<Finding>,
}

impl Symbols<'_> {
    fn inspect(&mut self, ident: &syn::Ident, span: Span) {
        let count = words(&ident.to_string()).len();
        if count < 4 {
            return;
        }
        let mut finding = Finding::error(self.rule, ident.to_string(), self.source.location(span));
        finding.message = format!("`{ident}` contains {count} semantic words; the maximum is three");
        finding.help = "reconsider the owning domain and module boundary, then name the declaration only for the concept not already supplied by that context".into();
        self.findings.push(finding);
    }
}

impl<'ast> Visit<'ast> for Symbols<'_> {
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let previous = self.trait_impl;
        self.trait_impl = item.trait_.is_some();
        syn::visit::visit_item_impl(self, item);
        self.trait_impl = previous;
    }

    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        self.inspect(&item.ident, item.span());
        syn::visit::visit_item_struct(self, item);
    }

    fn visit_item_enum(&mut self, item: &'ast ItemEnum) {
        self.inspect(&item.ident, item.span());
        syn::visit::visit_item_enum(self, item);
    }

    fn visit_item_trait(&mut self, item: &'ast ItemTrait) {
        self.inspect(&item.ident, item.span());
        syn::visit::visit_item_trait(self, item);
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        self.inspect(&item.sig.ident, item.span());
        syn::visit::visit_item_fn(self, item);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        if !self.trait_impl {
            self.inspect(&item.sig.ident, item.span());
        }
        syn::visit::visit_impl_item_fn(self, item);
    }

    fn visit_trait_item_fn(&mut self, item: &'ast TraitItemFn) {
        self.inspect(&item.sig.ident, item.span());
        syn::visit::visit_trait_item_fn(self, item);
    }
}

struct Prefixes<'a> {
    rule: &'static str,
    source: &'a Source,
    noun: String,
    trait_impl: bool,
    findings: &'a mut Vec<Finding>,
}

impl Prefixes<'_> {
    fn inspect(&mut self, ident: &syn::Ident, span: Span) {
        let tokens = words(&ident.to_string());
        if tokens.len() < 2 || tokens.first() != Some(&self.noun) {
            return;
        }
        let mut finding = Finding::error(self.rule, ident.to_string(), self.source.location(span));
        finding.message = format!("`{ident}` repeats its `{}` module noun", self.noun);
        finding.help = format!(
            "remove the `{}` prefix; the module path already supplies that context",
            self.noun
        );
        self.findings.push(finding);
    }
}

impl<'ast> Visit<'ast> for Prefixes<'_> {
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let previous = self.trait_impl;
        self.trait_impl = item.trait_.is_some();
        syn::visit::visit_item_impl(self, item);
        self.trait_impl = previous;
    }

    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        self.inspect(&item.ident, item.span());
        syn::visit::visit_item_struct(self, item);
    }

    fn visit_item_enum(&mut self, item: &'ast ItemEnum) {
        self.inspect(&item.ident, item.span());
        syn::visit::visit_item_enum(self, item);
    }

    fn visit_item_trait(&mut self, item: &'ast ItemTrait) {
        self.inspect(&item.ident, item.span());
        syn::visit::visit_item_trait(self, item);
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        self.inspect(&item.sig.ident, item.span());
        syn::visit::visit_item_fn(self, item);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        if !self.trait_impl {
            self.inspect(&item.sig.ident, item.span());
        }
        syn::visit::visit_impl_item_fn(self, item);
    }

    fn visit_trait_item_fn(&mut self, item: &'ast TraitItemFn) {
        self.inspect(&item.sig.ident, item.span());
        syn::visit::visit_trait_item_fn(self, item);
    }
}

fn conventional(stem: &str) -> bool {
    matches!(stem, "lib" | "main" | "mod" | "build")
}

fn numbered_fragment(stem: &str) -> bool {
    let stem = stem.to_ascii_lowercase();
    ["part", "section", "segment", "fragment", "chunk"].iter().any(|noun| {
        stem.strip_prefix(noun).is_some_and(|suffix| {
            let suffix = suffix.strip_prefix('_').unwrap_or(suffix);
            !suffix.is_empty() && suffix.chars().all(|character| character.is_ascii_digit())
        })
    })
}

fn module_directory(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    !matches!(
        name,
        "src" | "test" | "tests" | "test_support" | "benches" | "examples" | "packages" | "runtime" | "app"
    ) && !path.join("Cargo.toml").is_file()
}

fn action_form(word: &str) -> bool {
    matches!(
        word,
        "selected"
            | "prepared"
            | "creating"
            | "loading"
            | "running"
            | "handling"
            | "validating"
            | "decoding"
            | "encoding"
            | "dispatching"
            | "executing"
    )
}

fn module_noun(path: &Path) -> Option<String> {
    let parent = path.parent()?.file_name()?.to_str()?;
    if matches!(parent, "src" | "tests" | "benches" | "examples") {
        return None;
    }
    words(parent).into_iter().next()
}

fn file_location(path: &Path) -> Location {
    Location {
        path: path.to_owned(),
        line: 1,
        column: 1,
        source: String::new(),
    }
}

fn words(value: &str) -> Vec<String> {
    let characters = value.trim_start_matches("r#").chars().collect::<Vec<_>>();
    let mut words = Vec::new();
    let mut start = 0;
    for index in 0..characters.len() {
        let character = characters[index];
        if !character.is_ascii_alphanumeric() {
            push_word(&characters[start..index], &mut words);
            start = index + 1;
            continue;
        }
        if index == start {
            continue;
        }
        let previous = characters[index - 1];
        let next = characters.get(index + 1).copied();
        let boundary = character.is_ascii_uppercase()
            && (previous.is_ascii_lowercase()
                || previous.is_ascii_digit()
                || (previous.is_ascii_uppercase() && next.is_some_and(|next| next.is_ascii_lowercase())));
        if boundary {
            push_word(&characters[start..index], &mut words);
            start = index;
        }
    }
    push_word(&characters[start..], &mut words);
    words
}

fn push_word(characters: &[char], output: &mut Vec<String>) {
    let word = characters
        .iter()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    if !word.is_empty() {
        output.push(word);
    }
}
