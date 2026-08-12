use std::collections::{BTreeMap, HashMap, HashSet};

use quote::ToTokens;
use syn::{
    Attribute, Fields, GenericArgument, ImplItem, ItemImpl, ItemMod, ItemStruct, ItemType, PathArguments, Type,
    Visibility, spanned::Spanned, visit::Visit,
};

use crate::{
    Result,
    model::{Finding, Related, Review, Severity},
    rule::Rule,
    source::{Source, Workspace, requires_test},
};

mod dependencies;

#[cfg(test)]
#[path = "test.rs"]
mod tests;

use dependencies::local_dependencies;

/// Reviews likely copies of the same model across serialization boundaries.
pub struct Duplication;

impl Rule for Duplication {
    fn id(&self) -> &'static str {
        "wire-domain-model-duplication"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut database = Database::default();
        for source in workspace.production() {
            database.dependencies.extend(local_dependencies(&source.path)?);
            let mut aliases = AliasCollector::default();
            aliases.visit_file(&source.syntax);
            let mut collector = Collector::new(source, aliases.aliases);
            collector.visit_file(&source.syntax);
            database.definitions.extend(collector.definitions);
            database.conversions.extend(collector.conversions);
            for (name, count) in collector.behaviors {
                *database.behaviors.entry((source.package.clone(), name)).or_default() += count;
            }
        }
        Ok(database.findings(self.id()))
    }
}

#[derive(Default)]
struct Database {
    definitions: Vec<Definition>,
    conversions: Vec<Conversion>,
    behaviors: HashMap<(String, String), usize>,
    dependencies: HashSet<(String, String)>,
}

impl Database {
    fn findings(self, rule: &'static str) -> Vec<Finding> {
        let mut findings = Vec::new();
        for (index, first) in self.definitions.iter().enumerate() {
            for second in &self.definitions[index + 1..] {
                let conversion = self.conversions.iter().find(|item| item.connects(first, second));
                let Some((candidate, owner)) = self.roles(first, second, conversion.is_some()) else {
                    continue;
                };
                if !same_concept(candidate, owner, conversion.is_some()) {
                    continue;
                }
                let common = common_fields(candidate, owner);
                let smaller = candidate.fields.len().min(owner.fields.len());
                if common.len() < 3 || common.len() * 4 < smaller * 3 {
                    continue;
                }
                findings.push(finding(rule, candidate, owner, common, conversion));
            }
        }
        findings
    }

    fn roles<'a>(
        &self,
        first: &'a Definition,
        second: &'a Definition,
        converted: bool,
    ) -> Option<(&'a Definition, &'a Definition)> {
        if first.abi || second.abi || first.projection || second.projection {
            return None;
        }
        let first_depends_second = self
            .dependencies
            .contains(&(first.package.clone(), second.package.clone()));
        let second_depends_first = self
            .dependencies
            .contains(&(second.package.clone(), first.package.clone()));
        if first.package != second.package
            && first.domain != second.domain
            && !converted
            && !first_depends_second
            && !second_depends_first
        {
            return None;
        }
        let wire = |item: &Definition| item.serialized && item.public_fields >= 3;
        let domain = |item: &Definition| {
            !item.serialized
                && item.private_fields >= 3
                && self
                    .behaviors
                    .get(&(item.package.clone(), item.name.clone()))
                    .copied()
                    .unwrap_or_default()
                    > 0
        };
        if wire(first) && domain(second) {
            return Some((first, second));
        }
        if wire(second) && domain(first) {
            return Some((second, first));
        }
        if first.package != second.package && wire(first) && wire(second) {
            return match (first_depends_second, second_depends_first) {
                (true, false) => Some((first, second)),
                (false, true) => Some((second, first)),
                _ => Some((first, second)),
            };
        }
        None
    }
}

#[derive(Clone)]
struct Definition {
    package: String,
    domain: String,
    name: String,
    location: crate::Location,
    fields: BTreeMap<String, String>,
    serialized: bool,
    public_fields: usize,
    private_fields: usize,
    projection: bool,
    abi: bool,
}

#[derive(Clone)]
struct Conversion {
    package: String,
    from: String,
    to: String,
    kind: String,
    location: crate::Location,
}

impl Conversion {
    fn connects(&self, left: &Definition, right: &Definition) -> bool {
        (self.package == left.package || self.package == right.package)
            && ((self.from == left.name && self.to == right.name) || (self.from == right.name && self.to == left.name))
    }
}

struct Collector<'a> {
    source: &'a Source,
    test_scope: bool,
    aliases: HashMap<String, String>,
    definitions: Vec<Definition>,
    conversions: Vec<Conversion>,
    behaviors: HashMap<String, usize>,
}

impl<'a> Collector<'a> {
    fn new(source: &'a Source, aliases: HashMap<String, String>) -> Self {
        Self {
            source,
            test_scope: false,
            aliases,
            definitions: Vec::new(),
            conversions: Vec::new(),
            behaviors: HashMap::new(),
        }
    }
}

impl Visit<'_> for Collector<'_> {
    fn visit_item_mod(&mut self, item: &ItemMod) {
        let previous = self.test_scope;
        self.test_scope |= requires_test(&item.attrs);
        syn::visit::visit_item_mod(self, item);
        self.test_scope = previous;
    }

    fn visit_item_struct(&mut self, item: &ItemStruct) {
        if self.test_scope || requires_test(&item.attrs) {
            return;
        }
        let Fields::Named(fields) = &item.fields else {
            return;
        };
        if fields.named.len() < 3 {
            return;
        }
        let mut mapped = BTreeMap::new();
        let mut public_fields = 0;
        for field in &fields.named {
            let Some(ident) = &field.ident else {
                continue;
            };
            public_fields += usize::from(matches!(field.vis, Visibility::Public(_)));
            mapped.insert(
                serialized_name(ident.to_string(), &field.attrs),
                normalized_type(&field.ty, &self.aliases),
            );
        }
        let serialized = serialization(&item.attrs) || fields.named.iter().any(|field| serde_attribute(&field.attrs));
        self.definitions.push(Definition {
            package: self.source.package.clone(),
            domain: self.source.domain.clone(),
            name: item.ident.to_string(),
            location: self.source.location(item.span()),
            fields: mapped,
            serialized,
            public_fields,
            private_fields: fields.named.len() - public_fields,
            projection: projection_name(&item.ident.to_string()),
            abi: representation(&item.attrs),
        });
    }

    fn visit_item_impl(&mut self, item: &ItemImpl) {
        if self.test_scope || requires_test(&item.attrs) {
            return;
        }
        let Some(target) = simple_type(&item.self_ty) else {
            return;
        };
        if let Some((_, path, _)) = &item.trait_ {
            let Some(kind) = path.segments.last().map(|segment| segment.ident.to_string()) else {
                return;
            };
            if !matches!(kind.as_str(), "From" | "TryFrom") {
                return;
            }
            let Some(from) = path.segments.last().and_then(generic_type) else {
                return;
            };
            self.conversions.push(Conversion {
                package: self.source.package.clone(),
                from,
                to: target,
                kind,
                location: self.source.location(item.span()),
            });
            return;
        }
        let behavior = item
            .items
            .iter()
            .filter(|member| matches!(member, ImplItem::Fn(_)))
            .count();
        *self.behaviors.entry(target).or_default() += behavior;
    }
}

#[derive(Default)]
struct AliasCollector {
    aliases: HashMap<String, String>,
    test_scope: bool,
}

impl Visit<'_> for AliasCollector {
    fn visit_item_mod(&mut self, item: &ItemMod) {
        let previous = self.test_scope;
        self.test_scope |= requires_test(&item.attrs);
        syn::visit::visit_item_mod(self, item);
        self.test_scope = previous;
    }

    fn visit_item_type(&mut self, item: &ItemType) {
        if !self.test_scope && !requires_test(&item.attrs) {
            self.aliases
                .insert(item.ident.to_string(), normalized_type(&item.ty, &self.aliases));
        }
    }
}

fn same_concept(wire: &Definition, domain: &Definition, converted: bool) -> bool {
    converted || concept(&wire.name) == concept(&domain.name)
}

fn concept(name: &str) -> String {
    ["Wire", "Api", "Dto", "Model", "Data"]
        .iter()
        .fold(name.to_owned(), |name, token| name.replace(token, ""))
        .to_ascii_lowercase()
}

fn common_fields(wire: &Definition, domain: &Definition) -> Vec<(String, String)> {
    wire.fields
        .iter()
        .filter(|(name, ty)| domain.fields.get(*name) == Some(*ty))
        .map(|(name, ty)| (name.clone(), ty.clone()))
        .collect()
}

// The finding owns the field pairs it reports.
#[allow(clippy::needless_pass_by_value)]
fn finding(
    rule: &'static str,
    wire: &Definition,
    domain: &Definition,
    common: Vec<(String, String)>,
    conversion: Option<&Conversion>,
) -> Finding {
    let fields = common
        .iter()
        .map(|(name, ty)| format!("{name}: {ty}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut finding = Finding::warning(rule, format!("{}_{}", wire.name, domain.name), wire.location.clone());
    finding.message = format!(
        "`{}` appears to duplicate the owned `{}` model across a wire boundary",
        wire.name, domain.name
    );
    finding.help =
        "keep one owner for the model, or document the distinct wire contract and map only boundary-specific fields"
            .into();
    finding.related.push(Related {
        label: format!("candidate owning model; matching fields: {fields}"),
        location: domain.location.clone(),
    });
    if let Some(conversion) = conversion {
        finding.related.push(Related {
            label: format!("existing {} conversion copies between the models", conversion.kind),
            location: conversion.location.clone(),
        });
    }
    let mut review = Review::error();
    review.metadata.push(("Matching fields".into(), fields));
    review.metadata.push((
        "Wire evidence".into(),
        "serialization attributes and public fields".into(),
    ));
    review.metadata.push((
        "Owner evidence".into(),
        if domain.serialized {
            "local dependency direction or shared repository domain".into()
        } else {
            "private fields and inherent behavior".into()
        },
    ));
    review
        .questions
        .push("Is this a copied domain entity, or a deliberately narrower transport projection?".into());
    review.questions.push(
        "Can the protocol reuse the owned model or compose a stable shared value without leaking transport policy?"
            .into(),
    );
    finding.review = Some(review);
    finding
}

fn normalized_type(ty: &Type, aliases: &HashMap<String, String>) -> String {
    let rendered = ty.to_token_stream().to_string().replace(' ', "");
    aliases.get(&rendered).cloned().unwrap_or(rendered)
}

fn simple_type(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else {
        return None;
    };
    path.path.segments.last().map(|segment| segment.ident.to_string())
}

fn generic_type(segment: &syn::PathSegment) -> Option<String> {
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    arguments.args.iter().find_map(|argument| {
        let GenericArgument::Type(ty) = argument else {
            return None;
        };
        simple_type(ty)
    })
}

fn serialization(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        if attribute.path().is_ident("serde") {
            return true;
        }
        attribute.path().is_ident("derive")
            && attribute
                .meta
                .to_token_stream()
                .to_string()
                .split(|character: char| !character.is_ascii_alphanumeric())
                .any(|token| matches!(token, "Serialize" | "Deserialize"))
    })
}

fn serde_attribute(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| attribute.path().is_ident("serde"))
}

fn serialized_name(default: String, attributes: &[Attribute]) -> String {
    attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("serde"))
        .filter_map(serde_rename)
        .next_back()
        .unwrap_or(default)
}

fn serde_rename(attribute: &Attribute) -> Option<String> {
    let mut renamed = None;
    let _ = attribute.parse_nested_meta(|meta| {
        if !meta.path.is_ident("rename") {
            return Ok(());
        }
        renamed = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        Ok(())
    });
    renamed
}

fn representation(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| attribute.path().is_ident("repr"))
}

fn projection_name(name: &str) -> bool {
    ["Request", "Response", "View", "Summary", "Snapshot", "Event", "Command"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}
