use std::collections::BTreeSet;

use syn::{visit::Visit, ItemMod};

use crate::{
    model::{Finding, Location, Review, Severity},
    rule::Rule,
    source::{snake_case, Source, Workspace},
    Result,
};

const FORBIDDEN: &[&str] = &[
    "util", "utils", "core", "common", "shared", "helper", "helpers", "misc",
];

/// Rejects Rust modules whose names describe reuse rather than ownership.
pub struct CatchAllModule;

impl Rule for CatchAllModule {
    fn id(&self) -> &'static str {
        "catch-all-module-name"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        let mut file_modules = BTreeSet::new();
        let source_paths = workspace
            .sources()
            .iter()
            .map(|source| source.path.clone())
            .collect::<BTreeSet<_>>();

        for source in workspace.sources() {
            if let Some(name) = file_module_name(source) {
                if forbidden(&name) && file_modules.insert(source.path.clone()) {
                    findings.push(finding(
                        self.id(),
                        source,
                        name,
                        Location {
                            path: source.path.clone(),
                            line: 1,
                            column: 1,
                            source: String::new(),
                        },
                        "source file or module directory",
                    ));
                }
            }

            let mut visitor = ModuleVisitor {
                rule: self.id(),
                source,
                source_paths: &source_paths,
                findings: &mut findings,
            };
            visitor.visit_file(&source.syntax);
        }

        Ok(findings)
    }
}

struct ModuleVisitor<'a> {
    rule: &'static str,
    source: &'a Source,
    source_paths: &'a BTreeSet<std::path::PathBuf>,
    findings: &'a mut Vec<Finding>,
}

impl<'ast> Visit<'ast> for ModuleVisitor<'_> {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        let name = snake_case(&item.ident.to_string());
        let external_source_loaded = item.content.is_none()
            && module_source_paths(self.source, item, &name)
                .any(|path| self.source_paths.contains(&path));
        if forbidden(&name) && !external_source_loaded {
            self.findings.push(finding(
                self.rule,
                self.source,
                name,
                self.source.location(item.ident.span()),
                if item.content.is_some() {
                    "inline module declaration"
                } else {
                    "external module declaration"
                },
            ));
        }
        syn::visit::visit_item_mod(self, item);
    }
}

fn module_source_paths(
    source: &Source,
    item: &ItemMod,
    name: &str,
) -> impl Iterator<Item = std::path::PathBuf> {
    let directory = source
        .path
        .parent()
        .unwrap_or_else(|| std::path::Path::new(""));
    let explicit = item.attrs.iter().find_map(|attribute| {
        if !attribute.path().is_ident("path") {
            return None;
        }
        let syn::Meta::NameValue(value) = &attribute.meta else {
            return None;
        };
        let syn::Expr::Lit(value) = &value.value else {
            return None;
        };
        let syn::Lit::Str(path) = &value.lit else {
            return None;
        };
        Some(directory.join(path.value()))
    });
    explicit.into_iter().chain([
        directory.join(format!("{name}.rs")),
        directory.join(name).join("mod.rs"),
    ])
}

fn file_module_name(source: &Source) -> Option<String> {
    let stem = source.path.file_stem()?.to_str()?;
    let name = if stem == "mod" {
        source.path.parent()?.file_name()?.to_str()?
    } else {
        stem
    };
    // Crate roots and binary targets are targets, not module identifiers.
    (!matches!(name, "lib" | "main")).then(|| snake_case(name))
}

fn forbidden(name: &str) -> bool {
    FORBIDDEN.contains(&name)
}

fn finding(
    rule: &'static str,
    source: &Source,
    name: String,
    location: Location,
    declaration: &str,
) -> Finding {
    let mut finding = Finding::error(rule, name.clone(), location);
    finding.message = format!(
        "{declaration} `{name}` is a catch-all name that describes convenience or reuse, not ownership"
    );
    finding.help = "rename the module for the entity, capability, algorithm, fixture domain, or external mechanism it owns; split unrelated contents before renaming".to_owned();
    let mut review = Review::error();
    review.metadata = vec![
        ("domain".to_owned(), source.domain.clone()),
        ("package".to_owned(), source.package.clone()),
        ("module".to_owned(), name),
        ("declaration".to_owned(), declaration.to_owned()),
        (
            "scope".to_owned(),
            if source.test { "test" } else { "production" }.to_owned(),
        ),
    ];
    review.questions = vec![
        "Which single entity, capability, algorithm, fixture domain, or external mechanism owns these items?"
            .to_owned(),
        "Does the module mix unrelated responsibilities that must be split before it can receive a precise name?"
            .to_owned(),
    ];
    finding.review = Some(review);
    finding
}
