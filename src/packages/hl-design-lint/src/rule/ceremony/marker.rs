use std::collections::{BTreeMap, BTreeSet};

use syn::{
    spanned::Spanned, visit::Visit, GenericParam, ItemImpl, ItemTrait, Type, TypeParamBound,
    Visibility,
};

use crate::{
    model::{Finding, Related, Review},
    source::{requires_test, Source, Workspace},
};

use super::ID;

pub(super) fn findings(workspace: &Workspace) -> Vec<Finding> {
    let mut database = Database::default();
    for source in workspace.production() {
        let mut collector = Collector {
            source,
            test_scope: false,
            database: &mut database,
        };
        collector.visit_file(&source.syntax);
    }
    database.findings()
}

#[derive(Default)]
struct Database {
    traits: BTreeMap<(String, String), Vec<Trait>>,
    uses: BTreeSet<(String, String)>,
    impls: BTreeMap<(String, String), Vec<Implementation>>,
}

impl Database {
    fn findings(self) -> Vec<Finding> {
        self.traits
            .into_iter()
            .filter_map(|(key, mut definitions)| {
                if definitions.len() != 1 || self.uses.contains(&key) {
                    return None;
                }
                let definition = definitions.pop()?;
                let implementations = self.impls.get(&key).cloned().unwrap_or_default();
                if implementations.iter().any(|item| !item.blanket) {
                    return None;
                }
                definition.finding(implementations)
            })
            .collect()
    }
}

struct Trait {
    name: String,
    location: crate::Location,
}

impl Trait {
    fn finding(self, implementations: Vec<Implementation>) -> Option<Finding> {
        let proof = if implementations.is_empty() {
            "the private empty trait has no implementations or uses"
        } else {
            "the private empty trait is implemented only by unconstrained blanket implementations and is never used as a bound or trait object"
        };
        let mut finding = Finding::warning(ID, self.name.clone(), self.location);
        finding.message = format!("marker trait `{}` is ceremonial: {proof}", self.name);
        finding.help = "remove the trait, or give it a selective compile-time tagging/sealing contract that consumers actually use".into();
        finding.related = implementations
            .iter()
            .map(|implementation| Related {
                label: "unconstrained blanket implementation".into(),
                location: implementation.location.clone(),
            })
            .collect();
        let mut review = Review::error();
        review.metadata = vec![
            ("Category".into(), "unused empty marker trait".into()),
            ("Proof".into(), proof.into()),
            ("Implementations".into(), implementations.len().to_string()),
            ("Bound or trait-object uses".into(), "none".into()),
        ];
        review.questions = vec![
            "Does this marker provide a sealing, safety, auto-trait, or selective tag contract?"
                .into(),
            "Which consumer changes behavior based on this trait?".into(),
        ];
        finding.review = Some(review);
        Some(finding)
    }
}

#[derive(Clone)]
struct Implementation {
    blanket: bool,
    location: crate::Location,
}

struct Collector<'a, 'db> {
    source: &'a Source,
    test_scope: bool,
    database: &'db mut Database,
}

impl Visit<'_> for Collector<'_, '_> {
    fn visit_item_mod(&mut self, item: &syn::ItemMod) {
        let previous = self.test_scope;
        self.test_scope |= requires_test(&item.attrs);
        syn::visit::visit_item_mod(self, item);
        self.test_scope = previous;
    }

    fn visit_item_trait(&mut self, item: &ItemTrait) {
        if self.test_scope
            || requires_test(&item.attrs)
            || !matches!(item.vis, Visibility::Inherited)
            || !item.items.is_empty()
            || !item.supertraits.is_empty()
            || item.unsafety.is_some()
            || item.auto_token.is_some()
            || !item.attrs.is_empty()
        {
            return;
        }
        let name = item.ident.to_string();
        self.database
            .traits
            .entry((self.source.package.clone(), name.clone()))
            .or_default()
            .push(Trait {
                name,
                location: self.source.location(item.span()),
            });
    }

    fn visit_item_impl(&mut self, item: &ItemImpl) {
        if self.test_scope || requires_test(&item.attrs) {
            return;
        }
        let Some((_, path, _)) = &item.trait_ else {
            syn::visit::visit_item_impl(self, item);
            return;
        };
        let Some(name) = path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
        else {
            return;
        };
        let generic_names = item
            .generics
            .params
            .iter()
            .filter_map(|parameter| match parameter {
                GenericParam::Type(parameter) if parameter.bounds.is_empty() => {
                    Some(parameter.ident.to_string())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let blanket =
            simple_type(&item.self_ty).is_some_and(|owner| generic_names.contains(&owner));
        self.database
            .impls
            .entry((self.source.package.clone(), name))
            .or_default()
            .push(Implementation {
                blanket,
                location: self.source.location(item.span()),
            });
        syn::visit::visit_item_impl(self, item);
    }

    fn visit_trait_bound(&mut self, bound: &syn::TraitBound) {
        if let Some(name) = bound
            .path
            .segments
            .last()
            .map(|part| part.ident.to_string())
        {
            self.database
                .uses
                .insert((self.source.package.clone(), name));
        }
        syn::visit::visit_trait_bound(self, bound);
    }

    fn visit_type_trait_object(&mut self, object: &syn::TypeTraitObject) {
        for bound in &object.bounds {
            if let TypeParamBound::Trait(bound) = bound {
                if let Some(name) = bound
                    .path
                    .segments
                    .last()
                    .map(|part| part.ident.to_string())
                {
                    self.database
                        .uses
                        .insert((self.source.package.clone(), name));
                }
            }
        }
        syn::visit::visit_type_trait_object(self, object);
    }
}

fn simple_type(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else {
        return None;
    };
    (path.qself.is_none() && path.path.segments.len() == 1)
        .then(|| path.path.segments[0].ident.to_string())
}
