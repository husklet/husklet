use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use syn::{Attribute, Expr, Item, ItemMod, ItemUse, Lit, Meta, UseTree, spanned::Spanned};

use crate::{
    Result,
    model::{Finding, Location, Review, Severity},
    rule::Rule,
    source::{Source, Workspace, requires_test},
};

#[cfg(test)]
#[path = "test.rs"]
mod tests;

/// Rejects source directories made entirely from detached test fragments.
pub struct Directory;

impl Rule for Directory {
    fn id(&self) -> &'static str {
        "test-only-source-directory"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let declarations = Declarations::new(workspace);
        let mut directories = BTreeMap::<PathBuf, Vec<&Source>>::new();
        for source in workspace.sources() {
            if let Some(parent) = source.path.parent() {
                directories.entry(parent.to_owned()).or_default().push(source);
            }
        }
        Ok(directories
            .into_iter()
            .filter_map(|(path, sources)| {
                (sources.len() >= 2 && sources.iter().all(|source| declarations.test_source(source)))
                    .then(|| directory_finding(self.id(), &path, &sources))
            })
            .collect())
    }
}

/// Rejects tests coupled through another sibling test implementation.
pub struct Dependency;

impl Rule for Dependency {
    fn id(&self) -> &'static str {
        "sibling-test-dependency"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let declarations = Declarations::new(workspace);
        let mut findings = Vec::new();
        for source in workspace.sources() {
            inspect_dependencies(self.id(), source, &declarations, &mut findings);
        }
        let mut seen = BTreeSet::new();
        findings.retain(|finding| seen.insert((finding.location.path.clone(), finding.subject.clone())));
        Ok(findings)
    }
}

struct Declaration {
    name: String,
    target: PathBuf,
    test: bool,
}

struct Declarations {
    values: Vec<Declaration>,
}

impl Declarations {
    fn new(workspace: &Workspace) -> Self {
        let paths = workspace
            .sources()
            .iter()
            .map(|source| source.path.clone())
            .collect::<BTreeSet<_>>();
        let mut values = Vec::new();
        for source in workspace.sources() {
            for item in &source.syntax.items {
                let Item::Mod(module) = item else { continue };
                if let Some(target) = module_target(source, module, &paths) {
                    values.push(Declaration {
                        name: module.ident.to_string(),
                        target,
                        test: requires_test(&module.attrs),
                    });
                }
            }
        }
        Self { values }
    }

    fn test_source(&self, source: &Source) -> bool {
        let stem = source
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if stem == "test" || stem.ends_with("_test") || stem.ends_with("_tests") {
            return true;
        }
        let declarations = self
            .values
            .iter()
            .filter(|value| value.target == source.path)
            .collect::<Vec<_>>();
        !declarations.is_empty() && declarations.iter().all(|value| value.test)
    }

    fn sibling_tests(&self, source: &Source) -> BTreeSet<String> {
        let Some(parent) = source.path.parent() else {
            return BTreeSet::new();
        };
        self.values
            .iter()
            .filter(|value| value.test && value.target.parent() == Some(parent))
            .map(|value| value.name.clone())
            .collect()
    }
}

fn module_target(source: &Source, module: &ItemMod, paths: &BTreeSet<PathBuf>) -> Option<PathBuf> {
    let parent = source.path.parent()?;
    if let Some(path) = explicit_path(&module.attrs) {
        return Some(parent.join(path));
    }
    let name = module.ident.to_string();
    let file = parent.join(format!("{name}.rs"));
    if paths.contains(&file) {
        return Some(file);
    }
    let nested = parent.join(name).join("mod.rs");
    paths.contains(&nested).then_some(nested)
}

fn explicit_path(attributes: &[Attribute]) -> Option<String> {
    attributes.iter().find_map(|attribute| {
        if !attribute.path().is_ident("path") {
            return None;
        }
        let Meta::NameValue(value) = &attribute.meta else {
            return None;
        };
        let Expr::Lit(value) = &value.value else { return None };
        let Lit::Str(path) = &value.lit else { return None };
        Some(path.value())
    })
}

fn directory_finding(rule: &'static str, path: &Path, sources: &[&Source]) -> Finding {
    let names = sources
        .iter()
        .filter_map(|source| source.path.file_name()?.to_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut finding = Finding::error(
        rule,
        path.display().to_string(),
        Location {
            path: path.to_owned(),
            line: 1,
            column: 1,
            source: String::new(),
        },
    );
    finding.message = format!("source directory contains only detached test fragments: {names}");
    finding.help = "move each test beside its production module, prefer an inline #[cfg(test)] module, and put genuinely shared fixtures behind one noun-owned test_support module".into();
    let mut review = Review::error();
    review.metadata = vec![("test fragments".into(), sources.len().to_string())];
    review.questions = vec!["Which production noun owns each behavior under test?".into()];
    finding.review = Some(review);
    finding
}

fn inspect_dependencies(rule: &'static str, source: &Source, declarations: &Declarations, findings: &mut Vec<Finding>) {
    let mut siblings = declarations.sibling_tests(source);
    siblings.extend(source.syntax.items.iter().filter_map(|item| {
        let Item::Mod(module) = item else { return None };
        requires_test(&module.attrs).then(|| module.ident.to_string())
    }));
    if declarations.test_source(source) {
        inspect_items(rule, source, &source.syntax.items, &siblings, findings);
    }
    for item in &source.syntax.items {
        let Item::Mod(module) = item else { continue };
        if requires_test(&module.attrs) {
            if let Some((_, items)) = &module.content {
                inspect_items(rule, source, items, &siblings, findings);
            }
        }
    }
}

fn inspect_items(
    rule: &'static str,
    source: &Source,
    items: &[Item],
    siblings: &BTreeSet<String>,
    findings: &mut Vec<Finding>,
) {
    for item in items {
        let Item::Use(import) = item else { continue };
        for path in use_paths(import) {
            let Some(target) = path.iter().find(|part| siblings.contains(*part)) else {
                continue;
            };
            let target = (*target).clone();
            if matches!(target.as_str(), "support" | "test_support") {
                continue;
            }
            let mut finding = Finding::error(rule, target.clone(), source.location(import.span()));
            finding.message = format!("test code imports sibling test module `{target}`");
            finding.help = "test the production API independently; move intentionally shared fixtures into one noun-owned test_support module with no test behavior".into();
            findings.push(finding);
        }
    }
}

fn use_paths(item: &ItemUse) -> Vec<Vec<String>> {
    let mut output = Vec::new();
    flatten_use(&item.tree, Vec::new(), &mut output);
    output
}

fn flatten_use(tree: &UseTree, mut prefix: Vec<String>, output: &mut Vec<Vec<String>>) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            flatten_use(&path.tree, prefix, output);
        }
        UseTree::Name(name) => {
            prefix.push(name.ident.to_string());
            output.push(prefix);
        }
        UseTree::Rename(rename) => {
            prefix.push(rename.ident.to_string());
            output.push(prefix);
        }
        UseTree::Glob(_) => output.push(prefix),
        UseTree::Group(group) => {
            for item in &group.items {
                flatten_use(item, prefix.clone(), output);
            }
        }
    }
}
