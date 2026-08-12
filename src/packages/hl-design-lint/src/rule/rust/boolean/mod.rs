use std::collections::{BTreeMap, BTreeSet, HashMap};

use syn::{
    BinOp, Expr, ExprAssign, ExprBinary, ExprLit, ExprStruct, Fields, ImplItem, ItemImpl, ItemMod, ItemStruct, Lit,
    Member, Type, spanned::Spanned, visit::Visit,
};

use crate::{
    Result,
    model::{Finding, Related, Review, Severity},
    rule::{Rule, support::syntax::type_name},
    source::{Source, Workspace, requires_test},
};

#[cfg(test)]
#[path = "test.rs"]
mod tests;

/// Reviews related boolean fields that encode one mutually exclusive state.
pub struct State;

impl Rule for State {
    fn id(&self) -> &'static str {
        "boolean-state-cluster"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for source in workspace.production() {
            let mut collector = Collector::new(source);
            // Definitions must be known before evidence: Rust permits impls and
            // constructors to appear before their struct declaration.
            collector.visit_file(&source.syntax);
            collector.literals.clear();
            collector.transitions.clear();
            collector.visit_file(&source.syntax);
            findings.extend(collector.findings(self.id()));
        }
        Ok(findings)
    }
}

#[derive(Clone)]
struct Definition {
    name: String,
    fields: BTreeSet<String>,
    location: crate::Location,
}

#[derive(Clone)]
struct Evidence {
    label: String,
    location: crate::Location,
    fields: BTreeSet<String>,
}

struct Construction {
    values: BTreeMap<String, bool>,
    location: crate::Location,
}

struct Collector<'a> {
    source: &'a Source,
    modules: Vec<String>,
    test_scope: bool,
    definitions: BTreeMap<String, Definition>,
    literals: HashMap<String, Vec<Construction>>,
    transitions: HashMap<String, Vec<Evidence>>,
}

impl<'a> Collector<'a> {
    fn new(source: &'a Source) -> Self {
        Self {
            source,
            modules: Vec::new(),
            test_scope: false,
            definitions: BTreeMap::new(),
            literals: HashMap::new(),
            transitions: HashMap::new(),
        }
    }

    fn key(&self, name: &str) -> String {
        if self.modules.is_empty() {
            name.to_owned()
        } else {
            format!("{}::{name}", self.modules.join("::"))
        }
    }

    fn findings(mut self, rule: &'static str) -> Vec<Finding> {
        self.collect_literal_evidence();
        self.definitions
            .into_iter()
            .filter_map(|(key, definition)| {
                let mut evidence = self.transitions.remove(&key).unwrap_or_default();
                if evidence.is_empty() {
                    return None;
                }
                evidence.sort_by_key(|item| (item.location.path.clone(), item.location.line));
                let implicated = evidence
                    .iter()
                    .flat_map(|item| item.fields.iter().cloned())
                    .collect::<BTreeSet<_>>();
                let fields = implicated.iter().cloned().collect::<Vec<_>>().join(", ");
                let mut finding =
                    Finding::warning(rule, definition.name.clone(), definition.location);
                finding.message = format!(
                    "`{}` coordinates boolean fields as mutually exclusive state: {fields}",
                    definition.name
                );
                finding.help = "replace the related booleans with a named enum or composed state entity; keep independent capabilities as booleans".into();
                finding.related = evidence
                    .iter()
                    .map(|item| Related {
                        label: item.label.clone(),
                        location: item.location.clone(),
                    })
                    .collect();
                let mut review = Review::error();
                review.metadata.push(("Boolean fields".into(), fields));
                review.metadata.push((
                    "Evidence".into(),
                    evidence
                        .iter()
                        .map(|item| item.label.as_str())
                        .collect::<Vec<_>>()
                        .join("; "),
                ));
                review.questions.push(
                    "Do these flags represent one closed state rather than independent capabilities?"
                        .into(),
                );
                review.questions.push(
                    "Can invalid combinations be removed with an enum or specialized state value?".into(),
                );
                finding.review = Some(review);
                Some(finding)
            })
            .collect()
    }

    fn collect_literal_evidence(&mut self) {
        for (key, literals) in &self.literals {
            let Some(definition) = self.definitions.get(key) else {
                continue;
            };
            let complete = literals
                .iter()
                .filter(|construction| {
                    definition
                        .fields
                        .iter()
                        .all(|field| construction.values.contains_key(field))
                })
                .collect::<Vec<_>>();
            if complete.len() != literals.len()
                || complete.iter().any(|construction| {
                    definition
                        .fields
                        .iter()
                        .filter(|field| construction.values.get(*field) == Some(&true))
                        .count()
                        != 1
                })
            {
                continue;
            }
            let states = complete
                .iter()
                .filter_map(|construction| {
                    definition
                        .fields
                        .iter()
                        .find(|field| construction.values.get(*field) == Some(&true))
                        .cloned()
                })
                .collect::<BTreeSet<_>>();
            if states.len() < 2 {
                continue;
            }
            self.transitions
                .entry(key.clone())
                .or_default()
                .extend(complete.into_iter().take(4).map(|construction| Evidence {
                    label: "one-hot construction of the same boolean field set".into(),
                    location: construction.location.clone(),
                    fields: definition.fields.clone(),
                }));
        }
    }
}

impl Visit<'_> for Collector<'_> {
    fn visit_item_mod(&mut self, item: &ItemMod) {
        let previous = self.test_scope;
        self.test_scope |= requires_test(&item.attrs);
        if let Some((_, items)) = &item.content {
            self.modules.push(item.ident.to_string());
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
            .filter(|field| is_bool(&field.ty))
            .filter_map(|field| field.ident.as_ref().map(ToString::to_string))
            .collect::<BTreeSet<_>>();
        if fields.len() >= 3 {
            self.definitions.insert(
                self.key(&item.ident.to_string()),
                Definition {
                    name: item.ident.to_string(),
                    fields,
                    location: self.source.location(item.span()),
                },
            );
        }
    }

    fn visit_expr_struct(&mut self, item: &ExprStruct) {
        if !self.test_scope
            && let Some(name) = item.path.segments.last().map(|segment| segment.ident.to_string())
        {
            let values = item
                .fields
                .iter()
                .filter_map(|field| {
                    let Member::Named(name) = &field.member else {
                        return None;
                    };
                    literal_bool(&field.expr).map(|value| (name.to_string(), value))
                })
                .collect();
            self.literals.entry(self.key(&name)).or_default().push(Construction {
                values,
                location: self.source.location(item.span()),
            });
        }
        syn::visit::visit_expr_struct(self, item);
    }

    fn visit_item_impl(&mut self, item: &ItemImpl) {
        if self.test_scope || requires_test(&item.attrs) || item.trait_.is_some() {
            return;
        }
        let Some(name) = type_name(&item.self_ty) else {
            return;
        };
        let key = self.key(&name);
        let Some(definition) = self.definitions.get(&key).cloned() else {
            syn::visit::visit_item_impl(self, item);
            return;
        };
        for member in &item.items {
            let ImplItem::Fn(method) = member else {
                continue;
            };
            let mut assignments = Assignments::default();
            assignments.visit_block(&method.block);
            let coordinated = assignments
                .values
                .keys()
                .filter(|field| definition.fields.contains(*field))
                .cloned()
                .collect::<BTreeSet<_>>();
            let true_count = coordinated
                .iter()
                .filter(|field| assignments.values.get(*field) == Some(&true))
                .count();
            if coordinated.len() >= 3 && true_count == 1 {
                self.transitions.entry(key.clone()).or_default().push(Evidence {
                    label: format!(
                        "method `{}` coordinates {} boolean state fields",
                        method.sig.ident,
                        coordinated.len()
                    ),
                    location: self.source.location(method.span()),
                    fields: coordinated,
                });
            }
            let mut exclusions = Exclusions::default();
            exclusions.visit_block(&method.block);
            let excluded_fields = exclusions
                .pairs
                .iter()
                .flat_map(|(left, right)| [left.clone(), right.clone()])
                .filter(|field| definition.fields.contains(field))
                .collect::<BTreeSet<_>>();
            if excluded_fields.len() >= 3 && exclusions.pairs.len() >= 2 {
                self.transitions.entry(key.clone()).or_default().push(Evidence {
                    label: format!("method `{}` rejects mutually active boolean fields", method.sig.ident),
                    location: self.source.location(method.span()),
                    fields: excluded_fields,
                });
            }
        }
    }
}

#[derive(Default)]
struct Assignments {
    values: BTreeMap<String, bool>,
}

impl Visit<'_> for Assignments {
    fn visit_expr_assign(&mut self, item: &ExprAssign) {
        if let Expr::Field(field) = item.left.as_ref()
            && matches!(field.base.as_ref(), Expr::Path(path) if path.path.is_ident("self"))
            && let Member::Named(name) = &field.member
            && let Some(value) = literal_bool(&item.right)
        {
            self.values.insert(name.to_string(), value);
        }
        syn::visit::visit_expr_assign(self, item);
    }
}

#[derive(Default)]
struct Exclusions {
    pairs: BTreeSet<(String, String)>,
}

impl Visit<'_> for Exclusions {
    fn visit_expr_binary(&mut self, item: &ExprBinary) {
        if matches!(item.op, BinOp::And(_))
            && let (Some(left), Some(right)) = (self_field(&item.left), self_field(&item.right))
        {
            let pair = if left < right { (left, right) } else { (right, left) };
            self.pairs.insert(pair);
        }
        syn::visit::visit_expr_binary(self, item);
    }
}

fn self_field(expr: &Expr) -> Option<String> {
    let Expr::Field(field) = expr else {
        return None;
    };
    if !matches!(field.base.as_ref(), Expr::Path(path) if path.path.is_ident("self")) {
        return None;
    }
    let Member::Named(name) = &field.member else {
        return None;
    };
    Some(name.to_string())
}

fn is_bool(ty: &Type) -> bool {
    matches!(ty, Type::Path(path) if path.qself.is_none() && path.path.is_ident("bool"))
}

fn literal_bool(expr: &Expr) -> Option<bool> {
    let Expr::Lit(ExprLit {
        lit: Lit::Bool(value), ..
    }) = expr
    else {
        return None;
    };
    Some(value.value)
}
