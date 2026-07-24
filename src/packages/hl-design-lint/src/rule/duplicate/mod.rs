use std::collections::HashMap;

use proc_macro2::Span;
use syn::{spanned::Spanned, visit::Visit, Fields, ItemMod, ItemStruct};

use crate::{
    model::{Finding, Related, Review, Severity},
    rule::Rule,
    source::{requires_test, Source, Workspace},
    Result,
};

/// Reports related structs that repeat at least three identically typed fields.
pub struct DuplicateEntity;

impl Rule for DuplicateEntity {
    fn id(&self) -> &'static str {
        "duplicate-entity-base"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut definitions = Vec::new();
        for source in workspace.production() {
            let mut structs = Structs {
                package: &source.package,
                source,
                modules: Vec::new(),
                test_scope: false,
                definitions: Vec::new(),
            };
            structs.visit_file(&source.syntax);
            definitions.extend(structs.definitions);
        }
        Ok(compare(definitions)
            .into_iter()
            .map(|pair| pair.finding(self.id()))
            .collect())
    }
}

#[derive(Clone)]
struct Definition {
    package: String,
    module: String,
    location: crate::Location,
    name: String,
    fields: HashMap<String, String>,
}

struct Structs<'a> {
    package: &'a str,
    source: &'a Source,
    modules: Vec<String>,
    test_scope: bool,
    definitions: Vec<Definition>,
}

impl Visit<'_> for Structs<'_> {
    fn visit_item_mod(&mut self, module: &ItemMod) {
        let previous = self.test_scope;
        self.test_scope |= requires_test(&module.attrs);
        if let Some((_, items)) = &module.content {
            self.modules.push(module.ident.to_string());
            for item in items {
                self.visit_item(item);
            }
            self.modules.pop();
        }
        self.test_scope = previous;
    }

    fn visit_item_struct(&mut self, item: &ItemStruct) {
        if self.test_scope || requires_test(&item.attrs) {
            return;
        }
        let Fields::Named(fields) = &item.fields else {
            return;
        };
        let fields = fields
            .named
            .iter()
            .filter_map(|field| {
                Some((
                    field.ident.as_ref()?.to_string(),
                    normalized(self.source, field.ty.span()),
                ))
            })
            .collect::<HashMap<_, _>>();
        if fields.len() < 3 {
            return;
        }
        self.definitions.push(Definition {
            package: self.package.to_owned(),
            module: self.modules.join("::"),
            location: self.source.location(item.span()),
            name: item.ident.to_string(),
            fields,
        });
    }
}

fn normalized(source: &Source, span: Span) -> String {
    source
        .excerpt(span)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn related(first: &Definition, second: &Definition) -> bool {
    if first.name == second.name {
        return true;
    }
    let first_name = first.name.to_ascii_lowercase();
    let second_name = second.name.to_ascii_lowercase();
    if first_name.ends_with(&second_name) || second_name.ends_with(&first_name) {
        return true;
    }
    let namespace = first.module.rsplit("::").next().unwrap_or_default();
    first.module == second.module
        && (namespace.eq_ignore_ascii_case(&first.name)
            || namespace.eq_ignore_ascii_case(&second.name))
}

struct Pair {
    first: Definition,
    second: Definition,
    common: Vec<(String, String)>,
}

impl Pair {
    fn finding(self, rule: &'static str) -> Finding {
        let fields = self
            .common
            .iter()
            .map(|(name, ty)| format!("{name}: {ty}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut finding = Finding::error(
            rule,
            format!("{}_{}", self.first.name, self.second.name),
            self.first.location,
        );
        finding.message = format!(
            "`{}` and `{}` repeat a possible entity basis",
            self.first.name, self.second.name
        );
        finding.help = "extract a shared base entity and compose specialization, or prove the fields have different semantics".into();
        finding.related.push(Related {
            label: format!("second struct; common fields: {fields}"),
            location: self.second.location,
        });
        let mut review = Review::error();
        review.metadata.push(("Common fields".into(), fields));
        review
            .questions
            .push("Do these fields share identity, invariants, lifecycle, and meaning?".into());
        finding.review = Some(review);
        finding
    }
}

fn compare(definitions: Vec<Definition>) -> Vec<Pair> {
    let mut pairs = Vec::new();
    let mut packages = HashMap::<String, Vec<Definition>>::new();
    for definition in definitions {
        packages
            .entry(definition.package.clone())
            .or_default()
            .push(definition);
    }
    for definitions in packages.values() {
        for (index, first) in definitions.iter().enumerate() {
            for second in &definitions[index + 1..] {
                if !related(first, second) {
                    continue;
                }
                let mut common = first
                    .fields
                    .iter()
                    .filter(|(name, ty)| second.fields.get(*name) == Some(*ty))
                    .map(|(name, ty)| (name.clone(), ty.clone()))
                    .collect::<Vec<_>>();
                common.sort();
                if common.len() >= 3 {
                    pairs.push(Pair {
                        first: first.clone(),
                        second: second.clone(),
                        common,
                    });
                }
            }
        }
    }
    pairs
}
