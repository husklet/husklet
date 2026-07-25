use std::collections::{BTreeMap, BTreeSet};

use syn::{
    spanned::Spanned, visit::Visit, Expr, ExprField, ExprMethodCall, ImplItem, ImplItemFn,
    ItemImpl, ItemStruct, Member, Type,
};

use crate::{
    model::{Finding, Related, Review, Severity},
    rule::Rule,
    source::{requires_test, Workspace},
    Result,
};

const METHOD_THRESHOLD: usize = 20;
const CLUSTER_METHOD_THRESHOLD: usize = 2;
const CLUSTER_THRESHOLD: usize = 3;

mod origin;

#[cfg(test)]
mod tests;

/// Reviews state-owning types whose methods span several unrelated capabilities.
pub struct GodObject;

impl Rule for GodObject {
    fn id(&self) -> &'static str {
        "god-object-growth"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut types = definitions(workspace);
        collect_methods(workspace, &mut types);
        Ok(types.into_values().filter_map(finding).collect())
    }
}

struct TypeFacts {
    name: String,
    location: crate::Location,
    fields: BTreeSet<String>,
    origins: BTreeMap<String, String>,
    methods: Vec<Method>,
    excluded: bool,
}

struct Method {
    name: String,
    location: crate::Location,
    fields: BTreeSet<String>,
    calls: BTreeSet<String>,
    workflow: bool,
}

struct Cluster {
    fields: BTreeSet<String>,
    origins: BTreeSet<String>,
    methods: Vec<usize>,
}

fn definitions(workspace: &Workspace) -> BTreeMap<(String, String), TypeFacts> {
    let mut types = BTreeMap::new();
    let mut duplicate = BTreeSet::new();
    for source in workspace.production() {
        DefinitionCollector {
            source,
            types: &mut types,
            duplicate: &mut duplicate,
            test_scope: false,
        }
        .visit_file(&source.syntax);
    }
    for key in duplicate {
        types.remove(&key);
    }
    types
}

struct DefinitionCollector<'a, 'b> {
    source: &'a crate::Source,
    types: &'b mut BTreeMap<(String, String), TypeFacts>,
    duplicate: &'b mut BTreeSet<(String, String)>,
    test_scope: bool,
}

impl Visit<'_> for DefinitionCollector<'_, '_> {
    fn visit_item_mod(&mut self, item: &syn::ItemMod) {
        let previous = self.test_scope;
        self.test_scope |= requires_test(&item.attrs);
        syn::visit::visit_item_mod(self, item);
        self.test_scope = previous;
    }

    fn visit_item_struct(&mut self, item: &ItemStruct) {
        if self.test_scope || requires_test(&item.attrs) {
            return;
        }
        let name = item.ident.to_string();
        let key = (self.source.package.clone(), name.clone());
        if self.types.contains_key(&key) {
            self.duplicate.insert(key);
            return;
        }
        self.types.insert(
            key,
            TypeFacts {
                name,
                location: self.source.location(item.span()),
                fields: named_fields(item),
                origins: origin::field_origins(item),
                methods: Vec::new(),
                excluded: excluded_definition(item),
            },
        );
    }
}

fn named_fields(item: &ItemStruct) -> BTreeSet<String> {
    let syn::Fields::Named(fields) = &item.fields else {
        return BTreeSet::new();
    };
    fields
        .named
        .iter()
        .filter_map(|field| field.ident.as_ref().map(ToString::to_string))
        .collect()
}

fn excluded_definition(item: &ItemStruct) -> bool {
    item.ident.to_string().ends_with("Builder")
        || item.attrs.iter().any(|attribute| {
            attribute.path().is_ident("repr")
                && attribute
                    .meta
                    .require_list()
                    .is_ok_and(|list| list.tokens.to_string().contains('C'))
        })
}

fn collect_methods(workspace: &Workspace, types: &mut BTreeMap<(String, String), TypeFacts>) {
    for source in workspace.production() {
        MethodCollector {
            source,
            types,
            test_scope: false,
        }
        .visit_file(&source.syntax);
    }
}

struct MethodCollector<'a, 'b> {
    source: &'a crate::Source,
    types: &'b mut BTreeMap<(String, String), TypeFacts>,
    test_scope: bool,
}

impl Visit<'_> for MethodCollector<'_, '_> {
    fn visit_item_mod(&mut self, item: &syn::ItemMod) {
        let previous = self.test_scope;
        self.test_scope |= requires_test(&item.attrs);
        syn::visit::visit_item_mod(self, item);
        self.test_scope = previous;
    }

    fn visit_item_impl(&mut self, item: &ItemImpl) {
        if self.test_scope
            || item.trait_.is_some()
            || requires_test(&item.attrs)
            || item
                .attrs
                .iter()
                .any(|attribute| attribute.path().is_ident("automatically_derived"))
        {
            return;
        }
        let Some(name) = self_type_name(item) else {
            return;
        };
        let Some(facts) = self.types.get_mut(&(self.source.package.clone(), name)) else {
            return;
        };
        for member in &item.items {
            let ImplItem::Fn(method) = member else {
                continue;
            };
            if !has_receiver(method) || requires_test(&method.attrs) {
                continue;
            }
            let analysis = MethodAnalysis::analyze(method, &facts.fields);
            let workflow = analysis.workflow();
            facts.methods.push(Method {
                name: method.sig.ident.to_string(),
                location: self.source.location(method.span()),
                fields: analysis.fields,
                calls: analysis.calls,
                workflow,
            });
        }
    }
}

fn self_type_name(item: &ItemImpl) -> Option<String> {
    let Type::Path(path) = item.self_ty.as_ref() else {
        return None;
    };
    path.path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
}

fn has_receiver(method: &ImplItemFn) -> bool {
    method
        .sig
        .inputs
        .first()
        .is_some_and(|input| matches!(input, syn::FnArg::Receiver(_)))
}

fn finding(facts: TypeFacts) -> Option<Finding> {
    if facts.excluded || facts.methods.len() <= METHOD_THRESHOLD || facts.fields.len() < 3 {
        return None;
    }
    let clusters = clusters(&facts.methods, &facts.origins);
    if clusters.len() < CLUSTER_THRESHOLD {
        return None;
    }
    let crossing = facts.methods.iter().enumerate().find(|(_, method)| {
        let origins = method
            .calls
            .iter()
            .filter_map(|field| facts.origins.get(field))
            .collect::<BTreeSet<_>>();
        method.workflow && origins.len() >= 2
    })?;

    let descriptions = clusters
        .iter()
        .map(|cluster| describe_cluster(cluster, &facts.methods))
        .collect::<Vec<_>>();
    let mut finding = Finding::warning("god-object-growth", facts.name.clone(), facts.location);
    finding.message = format!(
        "`{}` owns {} inherent methods across {} distinct field capabilities; `{}` coordinates unrelated groups",
        facts.name,
        facts.methods.len(),
        clusters.len(),
        crossing.1.name,
    );
    finding.help = "extract the field-owned capabilities and their workflows; keep the root responsible only for construction and declarative cross-capability wiring".into();
    finding.related = clusters
        .iter()
        .map(|cluster| {
            let representative = cluster.methods[0];
            Related {
                label: describe_cluster(cluster, &facts.methods),
                location: facts.methods[representative].location.clone(),
            }
        })
        .chain(std::iter::once(Related {
            label: format!("cross-capability workflow `{}`", crossing.1.name),
            location: crossing.1.location.clone(),
        }))
        .collect();
    let mut review = Review::error();
    review
        .metadata
        .push(("Inherent methods".into(), facts.methods.len().to_string()));
    review
        .metadata
        .push(("Capability clusters".into(), descriptions.join("; ")));
    review
        .metadata
        .push(("Cross-capability method".into(), crossing.1.name.clone()));
    review.questions.push(
        "Do these field groups have separate invariants, lifecycles, or domain vocabularies?"
            .into(),
    );
    review.questions.push(
        "Can each group become a cohesive capability while this type retains only composition?"
            .into(),
    );
    finding.review = Some(review);
    Some(finding)
}

fn clusters(methods: &[Method], origins: &BTreeMap<String, String>) -> Vec<Cluster> {
    let mut by_fields: BTreeMap<Vec<String>, Vec<usize>> = BTreeMap::new();
    for (index, method) in methods.iter().enumerate() {
        if method.fields.is_empty()
            || method
                .calls
                .iter()
                .all(|field| !origins.contains_key(field))
        {
            continue;
        }
        by_fields
            .entry(method.fields.iter().cloned().collect())
            .or_default()
            .push(index);
    }
    let candidates = by_fields
        .into_iter()
        .filter(|(_, methods)| methods.len() >= CLUSTER_METHOD_THRESHOLD)
        .map(|(fields, methods)| Cluster {
            origins: fields
                .iter()
                .filter_map(|field| origins.get(field).cloned())
                .collect(),
            fields: fields.into_iter().collect(),
            methods,
        })
        .filter(|cluster| !cluster.origins.is_empty())
        .collect::<Vec<_>>();

    let minimal = candidates
        .iter()
        .enumerate()
        .filter(|(index, candidate)| {
            candidates.iter().enumerate().all(|(other_index, other)| {
                *index == other_index
                    || other.fields == candidate.fields
                    || !other.fields.is_subset(&candidate.fields)
            })
        })
        .map(|(_, cluster)| cluster)
        .collect::<Vec<_>>();

    let disjoint = minimal
        .iter()
        .enumerate()
        .filter(|(index, candidate)| {
            minimal.iter().enumerate().all(|(other_index, other)| {
                *index == other_index || candidate.fields.is_disjoint(&other.fields)
            })
        })
        .map(|(_, cluster)| Cluster {
            fields: cluster.fields.clone(),
            origins: cluster.origins.clone(),
            methods: cluster.methods.clone(),
        })
        .collect::<Vec<_>>();
    let distinct_origins = disjoint
        .iter()
        .flat_map(|cluster| cluster.origins.iter())
        .collect::<BTreeSet<_>>();
    if distinct_origins.len() < CLUSTER_THRESHOLD {
        return Vec::new();
    }
    disjoint
}

fn describe_cluster(cluster: &Cluster, methods: &[Method]) -> String {
    let fields = cluster
        .fields
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let examples = cluster
        .methods
        .iter()
        .take(3)
        .map(|index| methods[*index].name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let origins = cluster
        .origins
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    format!("fields [{fields}] from [{origins}] via [{examples}]")
}

struct MethodAnalysis<'a> {
    known_fields: &'a BTreeSet<String>,
    fields: BTreeSet<String>,
    calls: BTreeSet<String>,
    control: bool,
    assignment: bool,
}

impl<'a> MethodAnalysis<'a> {
    fn analyze(method: &ImplItemFn, known_fields: &'a BTreeSet<String>) -> Self {
        let mut analysis = Self {
            known_fields,
            fields: BTreeSet::new(),
            calls: BTreeSet::new(),
            control: false,
            assignment: false,
        };
        analysis.visit_block(&method.block);
        analysis
    }

    fn workflow(&self) -> bool {
        self.control || self.assignment
    }
}

impl<'ast> Visit<'ast> for MethodAnalysis<'_> {
    fn visit_expr_method_call(&mut self, expression: &'ast ExprMethodCall) {
        if let Some(field) = receiver_field(&expression.receiver) {
            if self.known_fields.contains(&field) {
                self.calls.insert(field);
            }
        }
        syn::visit::visit_expr_method_call(self, expression);
    }

    fn visit_expr_field(&mut self, expression: &'ast ExprField) {
        if matches!(expression.base.as_ref(), Expr::Path(path) if path.path.is_ident("self")) {
            if let Member::Named(field) = &expression.member {
                let field = field.to_string();
                if self.known_fields.contains(&field) {
                    self.fields.insert(field);
                }
            }
        }
        syn::visit::visit_expr_field(self, expression);
    }

    fn visit_expr(&mut self, expression: &'ast Expr) {
        match expression {
            Expr::Assign(_) => self.assignment = true,
            Expr::ForLoop(_) | Expr::If(_) | Expr::Loop(_) | Expr::Match(_) | Expr::While(_) => {
                self.control = true
            }
            _ => {}
        }
        syn::visit::visit_expr(self, expression);
    }
}

fn receiver_field(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Field(field) if matches!(field.base.as_ref(), Expr::Path(path) if path.path.is_ident("self")) =>
        {
            let Member::Named(field) = &field.member else {
                return None;
            };
            Some(field.to_string())
        }
        Expr::Reference(reference) => receiver_field(&reference.expr),
        Expr::Paren(paren) => receiver_field(&paren.expr),
        _ => None,
    }
}
