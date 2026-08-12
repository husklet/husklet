use std::collections::{BTreeMap, BTreeSet};

use quote::ToTokens;
use syn::{Item, ItemMod, Visibility, spanned::Spanned, visit::Visit};

use crate::{
    Result,
    model::{Finding, Review, ReviewState, Severity},
    rule::Rule,
    source::{Source, Workspace, requires_test},
};

#[cfg(test)]
#[path = "test.rs"]
mod tests;

/// Reviews source tests that can be proven to use only the public crate surface.
pub struct IntegrationCandidate;

impl Rule for IntegrationCandidate {
    fn id(&self) -> &'static str {
        "integration-test-candidate"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let public = public_api(workspace);
        let mut findings = Vec::new();
        for source in workspace.sources() {
            let api = public.get(&source.package).cloned().unwrap_or_default();
            if source.test {
                inspect_unit(self.id(), source, &source.syntax.items, &api, &mut findings);
            }
            for item in &source.syntax.items {
                let Item::Mod(module) = item else { continue };
                if requires_test(&module.attrs)
                    && let Some((_, items)) = &module.content
                {
                    inspect_module(self.id(), source, module, items, &api, &mut findings);
                }
            }
        }
        Ok(findings)
    }
}

fn public_api(workspace: &Workspace) -> BTreeMap<String, BTreeSet<String>> {
    let mut packages = BTreeMap::<String, BTreeSet<String>>::new();
    for source in workspace
        .production()
        .filter(|source| source.path.file_name().and_then(|name| name.to_str()) == Some("lib.rs"))
    {
        let names = packages.entry(source.package.clone()).or_default();
        for item in &source.syntax.items {
            match item {
                Item::Const(value) if public_visibility(&value.vis) => {
                    names.insert(value.ident.to_string());
                }
                Item::Enum(value) if public_visibility(&value.vis) => {
                    names.insert(value.ident.to_string());
                }
                Item::Fn(value) if public_visibility(&value.vis) => {
                    names.insert(value.sig.ident.to_string());
                }
                Item::Mod(value) if public_visibility(&value.vis) => {
                    names.insert(value.ident.to_string());
                }
                Item::Static(value) if public_visibility(&value.vis) => {
                    names.insert(value.ident.to_string());
                }
                Item::Struct(value) if public_visibility(&value.vis) => {
                    names.insert(value.ident.to_string());
                }
                Item::Trait(value) if public_visibility(&value.vis) => {
                    names.insert(value.ident.to_string());
                }
                Item::Type(value) if public_visibility(&value.vis) => {
                    names.insert(value.ident.to_string());
                }
                Item::Union(value) if public_visibility(&value.vis) => {
                    names.insert(value.ident.to_string());
                }
                Item::Use(value) if public_visibility(&value.vis) => public_use_names(&value.tree, names),
                _ => {}
            }
        }
    }
    packages
}

fn public_visibility(visibility: &Visibility) -> bool {
    matches!(visibility, Visibility::Public(_))
}

fn public_use_names(tree: &syn::UseTree, names: &mut BTreeSet<String>) {
    match tree {
        syn::UseTree::Name(value) => {
            names.insert(value.ident.to_string());
        }
        syn::UseTree::Rename(value) => {
            names.insert(value.rename.to_string());
        }
        syn::UseTree::Path(value) => public_use_names(&value.tree, names),
        syn::UseTree::Group(value) => {
            for item in &value.items {
                public_use_names(item, names);
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}

fn inspect_module(
    rule: &'static str,
    source: &Source,
    module: &ItemMod,
    items: &[Item],
    public: &BTreeSet<String>,
    findings: &mut Vec<Finding>,
) {
    let before = findings.len();
    inspect_unit(rule, source, items, public, findings);
    if findings.len() > before {
        findings.last_mut().unwrap().location = source.location(module.span());
    }
}

fn inspect_unit(
    rule: &'static str,
    source: &Source,
    items: &[Item],
    public: &BTreeSet<String>,
    findings: &mut Vec<Finding>,
) {
    let mut evidence = Evidence {
        public,
        has_test: false,
        public_reference: false,
        private: false,
    };
    for item in items {
        evidence.visit_item(item);
    }
    if !evidence.has_test || !evidence.public_reference || evidence.private {
        return;
    }

    let subject = source.path.file_name().and_then(|name| name.to_str()).unwrap_or("test");
    let span = items.first().map_or_else(proc_macro2::Span::call_site, Item::span);
    let mut finding = Finding::warning(rule, subject, source.location(span));
    finding.message =
        "source unit tests use only the ordinary public crate API and are integration-test candidates".into();
    finding.help = "review whether cross-domain/public behavior belongs under the crate tests/ boundary; syntax proves candidacy only when no private dependency is visible, and this rule never moves code automatically".into();
    finding.review = Some(Review {
        state: ReviewState::Check("integration-placement-review".into()),
        metadata: vec![("evidence".into(), "public crate surface only".into())],
        dependencies: Vec::new(),
        questions: vec![
            "Does this test validate a public contract across domain boundaries?".into(),
            "Would moving it lose useful private invariant coverage?".into(),
        ],
    });
    findings.push(finding);
}

struct Evidence<'a> {
    public: &'a BTreeSet<String>,
    has_test: bool,
    public_reference: bool,
    private: bool,
}

impl Evidence<'_> {
    fn inspect_use(&mut self, tree: &syn::UseTree, prefix: &mut Vec<String>) {
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.inspect_use(&path.tree, prefix);
                prefix.pop();
            }
            syn::UseTree::Name(name) => self.inspect_import(prefix, &name.ident.to_string()),
            syn::UseTree::Rename(name) => self.inspect_import(prefix, &name.ident.to_string()),
            syn::UseTree::Group(group) => {
                for tree in &group.items {
                    self.inspect_use(tree, prefix);
                }
            }
            // A glob cannot prove that every referenced name is public.
            syn::UseTree::Glob(_) => self.private = true,
        }
    }

    fn inspect_import(&mut self, prefix: &[String], leaf: &str) {
        let first = prefix.first().map_or(leaf, String::as_str);
        if first == "super" || first == "self" {
            self.private = true;
        } else if first == "crate" {
            let root = prefix.get(1).map_or(leaf, String::as_str);
            if self.public.contains(root) {
                self.public_reference = true;
            } else {
                self.private = true;
            }
        }
    }
}

impl<'ast> Visit<'ast> for Evidence<'_> {
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if item.attrs.iter().any(|attribute| attribute.path().is_ident("test")) {
            self.has_test = true;
        }
        syn::visit::visit_item_fn(self, item);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let names = path
            .segments
            .iter()
            .map(|part| part.ident.to_string())
            .collect::<Vec<_>>();
        if names.first().is_some_and(|name| name == "super" || name == "self") {
            self.private = true;
        }
        if names.first().is_some_and(|name| name == "crate") {
            match names.get(1) {
                Some(name) if self.public.contains(name) => self.public_reference = true,
                _ => self.private = true,
            }
        }
        syn::visit::visit_path(self, path);
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.inspect_use(&item.tree, &mut Vec::new());
        syn::visit::visit_item_use(self, item);
    }

    fn visit_field_value(&mut self, field: &'ast syn::FieldValue) {
        self.private = true;
        syn::visit::visit_field_value(self, field);
    }

    fn visit_expr_field(&mut self, field: &'ast syn::ExprField) {
        self.private = true;
        syn::visit::visit_expr_field(self, field);
    }

    fn visit_visibility(&mut self, visibility: &'ast Visibility) {
        if matches!(visibility, Visibility::Restricted(_)) {
            self.private = true;
        }
        syn::visit::visit_visibility(self, visibility);
    }

    fn visit_macro(&mut self, value: &'ast syn::Macro) {
        let text = value.to_token_stream().to_string();
        if text.contains("super ::") || text.contains("crate ::") {
            self.private = true;
        }
        syn::visit::visit_macro(self, value);
    }
}
