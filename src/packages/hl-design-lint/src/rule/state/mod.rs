use std::collections::{BTreeMap, BTreeSet};

use syn::{
    BinOp, Expr, ExprAssign, ExprBinary, ExprField, ExprMatch, ExprMethodCall, ExprPath, ExprStruct, Fields,
    ImplItemFn, ItemFn, ItemImpl, ItemMod, ItemStruct, Member, visit::Visit,
};

use crate::{
    Result,
    model::{Finding, Related, Review, Severity},
    rule::Rule,
    source::{Source, Workspace, requires_test},
};

mod syntax;
use syntax::{
    candidate_name, excluded_name, is_self, pattern_literals, peel, preserves_unknown, string_literal, string_type,
    type_name,
};

/// Finds closed state vocabularies represented as string literals.
pub struct FiniteStateString;

impl Rule for FiniteStateString {
    fn id(&self) -> &'static str {
        "string-backed-finite-state"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut concepts = BTreeMap::<Concept, Evidence>::new();
        for source in workspace.production() {
            let mut visitor = Strings {
                source,
                concepts: &mut concepts,
                scope: Vec::new(),
                owner: None,
                test_scope: false,
                string_fields: BTreeSet::new(),
            };
            visitor.visit_file(&source.syntax);
        }

        Ok(concepts
            .into_iter()
            .filter_map(|(concept, evidence)| finding(self.id(), concept, evidence))
            .collect())
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Concept {
    package: String,
    identity: String,
    owner: String,
    name: String,
    kind: &'static str,
}

#[derive(Default)]
struct Evidence {
    literals: BTreeMap<String, Vec<crate::Location>>,
    assignments: usize,
    comparisons: usize,
    matches: usize,
    preserves_unknown: bool,
}

impl Evidence {
    fn record(&mut self, value: String, location: crate::Location, kind: EvidenceKind) {
        self.literals.entry(value).or_default().push(location);
        match kind {
            EvidenceKind::Assignment => self.assignments += 1,
            EvidenceKind::Comparison => self.comparisons += 1,
            EvidenceKind::Match => self.matches += 1,
        }
    }
}

#[derive(Clone, Copy)]
enum EvidenceKind {
    Assignment,
    Comparison,
    Match,
}

struct Strings<'a> {
    source: &'a Source,
    concepts: &'a mut BTreeMap<Concept, Evidence>,
    scope: Vec<String>,
    owner: Option<String>,
    test_scope: bool,
    string_fields: BTreeSet<(String, String)>,
}

impl Strings<'_> {
    fn concept(&self, expression: &Expr) -> Option<Concept> {
        match peel(expression) {
            Expr::Path(path) => self.path_concept(path),
            Expr::Field(field) => self.field_concept(field),
            _ => None,
        }
    }

    fn path_concept(&self, path: &ExprPath) -> Option<Concept> {
        if path.qself.is_some() || path.path.segments.len() != 1 {
            return None;
        }
        let name = path.path.segments.first()?.ident.to_string();
        (!excluded_name(&name) && candidate_name(&name)).then(|| Concept {
            package: self.source.package.clone(),
            identity: format!("{}::{}", self.source.path.display(), self.scope.join("::")),
            owner: self.scope.join("::"),
            name,
            kind: "binding",
        })
    }

    fn field_concept(&self, field: &ExprField) -> Option<Concept> {
        let Member::Named(member) = &field.member else {
            return None;
        };
        let name = member.to_string();
        if excluded_name(&name) || !candidate_name(&name) {
            return None;
        }
        let owner = self.owner.as_ref()?;
        if !is_self(&field.base) || !self.string_fields.contains(&(owner.clone(), name.clone())) {
            return None;
        }
        Some(Concept {
            package: self.source.package.clone(),
            identity: format!("{}::{owner}", self.source.path.display()),
            owner: owner.clone(),
            name,
            kind: "field",
        })
    }

    fn record(&mut self, expression: &Expr, literal: &Expr, kind: EvidenceKind) {
        let (value, span) = match string_literal(literal) {
            Some(value) => value,
            None => return,
        };
        let Some(concept) = self.concept(expression) else {
            return;
        };
        self.concepts
            .entry(concept)
            .or_default()
            .record(value, self.source.location(span), kind);
    }

    fn record_field(&mut self, owner: &str, name: &str, literal: &Expr, kind: EvidenceKind) {
        if excluded_name(name)
            || !candidate_name(name)
            || !self.string_fields.contains(&(owner.to_owned(), name.to_owned()))
        {
            return;
        }
        let Some((value, span)) = string_literal(literal) else {
            return;
        };
        let concept = Concept {
            package: self.source.package.clone(),
            identity: format!("{}::{owner}", self.source.path.display()),
            owner: owner.to_owned(),
            name: name.to_owned(),
            kind: "field",
        };
        self.concepts
            .entry(concept)
            .or_default()
            .record(value, self.source.location(span), kind);
    }

    fn record_target(&mut self, name: &str, literal: &Expr) {
        let Some(name) = name.strip_prefix("set_") else {
            return;
        };
        if excluded_name(name) || !candidate_name(name) {
            return;
        }
        let Some((value, span)) = string_literal(literal) else {
            return;
        };
        let owner = self.scope.join("::");
        let concept = Concept {
            package: self.source.package.clone(),
            identity: format!("{}::{owner}::set_{name}", self.source.path.display()),
            owner,
            name: name.to_owned(),
            kind: "target",
        };
        self.concepts
            .entry(concept)
            .or_default()
            .record(value, self.source.location(span), EvidenceKind::Assignment);
    }

    fn visit_scoped(&mut self, name: String, visit: impl FnOnce(&mut Self)) {
        self.scope.push(name);
        visit(self);
        self.scope.pop();
    }
}

impl<'ast> Visit<'ast> for Strings<'_> {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        let previous = self.test_scope;
        self.test_scope |= requires_test(&item.attrs);
        if !self.test_scope {
            self.visit_scoped(item.ident.to_string(), |visitor| {
                syn::visit::visit_item_mod(visitor, item);
            });
        }
        self.test_scope = previous;
    }

    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        if self.test_scope || requires_test(&item.attrs) {
            return;
        }
        let Fields::Named(fields) = &item.fields else {
            return;
        };
        for field in &fields.named {
            let Some(name) = &field.ident else {
                continue;
            };
            if string_type(&field.ty) {
                self.string_fields.insert((item.ident.to_string(), name.to_string()));
            }
        }
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        if self.test_scope || requires_test(&item.attrs) {
            return;
        }
        self.visit_scoped(item.sig.ident.to_string(), |visitor| {
            syn::visit::visit_item_fn(visitor, item);
        });
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        if self.test_scope || requires_test(&item.attrs) {
            return;
        }
        let previous = self.owner.take();
        self.owner = type_name(&item.self_ty);
        syn::visit::visit_item_impl(self, item);
        self.owner = previous;
    }

    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        if self.test_scope || requires_test(&item.attrs) {
            return;
        }
        self.visit_scoped(item.sig.ident.to_string(), |visitor| {
            syn::visit::visit_impl_item_fn(visitor, item);
        });
    }

    fn visit_expr_assign(&mut self, item: &'ast ExprAssign) {
        self.record(&item.left, &item.right, EvidenceKind::Assignment);
        syn::visit::visit_expr_assign(self, item);
    }

    fn visit_expr_struct(&mut self, item: &'ast ExprStruct) {
        if let Some(mut owner) = item.path.segments.last().map(|segment| segment.ident.to_string()) {
            if owner == "Self" {
                let Some(current) = self.owner.clone() else {
                    syn::visit::visit_expr_struct(self, item);
                    return;
                };
                owner = current;
            }
            for field in &item.fields {
                if let Member::Named(name) = &field.member {
                    self.record_field(&owner, &name.to_string(), &field.expr, EvidenceKind::Assignment);
                }
            }
        }
        syn::visit::visit_expr_struct(self, item);
    }

    fn visit_expr_binary(&mut self, item: &'ast ExprBinary) {
        if matches!(item.op, BinOp::Eq(_) | BinOp::Ne(_)) {
            self.record(&item.left, &item.right, EvidenceKind::Comparison);
            self.record(&item.right, &item.left, EvidenceKind::Comparison);
        }
        syn::visit::visit_expr_binary(self, item);
    }

    fn visit_expr_match(&mut self, item: &'ast ExprMatch) {
        if let Some(concept) = self.concept(&item.expr) {
            let open = item
                .arms
                .iter()
                .any(|arm| preserves_unknown(&arm.pat, &arm.body, &concept.name));
            for arm in &item.arms {
                for (value, span) in pattern_literals(&arm.pat) {
                    self.concepts.entry(concept.clone()).or_default().record(
                        value,
                        self.source.location(span),
                        EvidenceKind::Match,
                    );
                }
            }
            if open {
                self.concepts.entry(concept).or_default().preserves_unknown = true;
            }
        }
        syn::visit::visit_expr_match(self, item);
    }

    fn visit_expr_method_call(&mut self, item: &'ast ExprMethodCall) {
        if item.args.len() == 1 {
            if let Some(argument) = item.args.first() {
                self.record_target(&item.method.to_string(), argument);
            }
        }
        syn::visit::visit_expr_method_call(self, item);
    }
}

fn finding(rule: &'static str, concept: Concept, evidence: Evidence) -> Option<Finding> {
    if evidence.preserves_unknown {
        return None;
    }
    let decision = evidence.comparisons + evidence.matches > 0;
    let persistent_field = concept.kind == "field" && (evidence.assignments >= 3 || decision);
    let assignment_target = concept.kind == "target" && evidence.assignments >= 3;
    let evolving_binding = concept.kind == "binding" && evidence.assignments > 0 && decision;
    if evidence.literals.len() < 3 || !(persistent_field || assignment_target || evolving_binding) {
        return None;
    }
    let mut locations = evidence
        .literals
        .iter()
        .flat_map(|(literal, locations)| locations.iter().cloned().map(|location| (literal.clone(), location)))
        .collect::<Vec<_>>();
    locations.sort_by(|left, right| left.1.path.cmp(&right.1.path).then(left.1.line.cmp(&right.1.line)));
    let (_, location) = locations.first()?.clone();
    let values = evidence.literals.keys().cloned().collect::<Vec<_>>().join(", ");
    let subject = format!("{}::{}", concept.owner, concept.name);
    let mut finding = Finding::warning(rule, subject, location);
    finding.message = format!(
        "`{}` uses {} string literals as a finite state vocabulary",
        concept.name,
        evidence.literals.len()
    );
    finding.help =
        "model the closed vocabulary as an enum; serialize or parse strings only at wire/storage boundaries".into();
    finding.related = locations
        .into_iter()
        .skip(1)
        .take(12)
        .map(|(value, location)| Related {
            label: format!("`{value}` is another value for this {}", concept.kind),
            location,
        })
        .collect();
    let mut review = Review::error();
    review.metadata.push(("Concept".into(), concept.name));
    review.metadata.push(("Values".into(), values));
    review
        .metadata
        .push(("Assignments".into(), evidence.assignments.to_string()));
    review
        .metadata
        .push(("Comparisons".into(), evidence.comparisons.to_string()));
    review
        .metadata
        .push(("Match patterns".into(), evidence.matches.to_string()));
    review
        .questions
        .push("Is this vocabulary closed by domain policy, and where should its enum own parsing?".into());
    finding.review = Some(review);
    Some(finding)
}

#[cfg(test)]
#[path = "test.rs"]
mod tests;
