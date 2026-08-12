use std::collections::{BTreeMap, BTreeSet};

use quote::ToTokens;
use syn::{Expr, FnArg, ImplItem, Item, ItemImpl, ItemStruct, Member, Pat, Stmt, Type, Visibility, spanned::Spanned};

use crate::{
    model::{Finding, Related, Review},
    source::{Source, Workspace},
};

use super::ID;

const MINIMUM_FORWARDERS: usize = 3;

pub(super) fn findings(workspace: &Workspace) -> Vec<Finding> {
    let database = Database::collect(workspace);
    database
        .wrappers
        .values()
        .filter_map(|wrapper| wrapper.finding(&database))
        .collect()
}

#[derive(Default)]
struct Database {
    wrappers: BTreeMap<(String, String), Wrapper>,
    methods: BTreeMap<(String, String, String), Vec<Method>>,
    trait_owners: BTreeSet<(String, String)>,
    ambiguous: BTreeSet<(String, String)>,
}

impl Database {
    fn collect(workspace: &Workspace) -> Self {
        let mut database = Self::default();
        for source in workspace.production() {
            for item in &source.syntax.items {
                if let Item::Struct(item) = item {
                    database.structure(source, item);
                }
            }
        }
        for source in workspace.production() {
            for item in &source.syntax.items {
                if let Item::Impl(item) = item {
                    database.implementation(source, item);
                }
            }
        }
        database
    }

    fn structure(&mut self, source: &Source, item: &ItemStruct) {
        let name = item.ident.to_string();
        let key = (source.package.clone(), name.clone());
        if self.wrappers.contains_key(&key) {
            self.ambiguous.insert(key);
            return;
        }
        if public(&item.vis) || boundary_attrs(&item.attrs) || item.fields.len() != 1 {
            return;
        }
        let field = item.fields.iter().next().expect("one field");
        if boundary_attrs(&field.attrs) {
            return;
        }
        let Some(inner) = simple_type(&field.ty) else {
            return;
        };
        self.wrappers.insert(
            key,
            Wrapper {
                package: source.package.clone(),
                name,
                inner,
                field: field
                    .ident
                    .as_ref()
                    .map_or(Member::Unnamed(0.into()), |ident| Member::Named(ident.clone())),
                location: source.location(item.span()),
                methods: Vec::new(),
                invalid: false,
            },
        );
    }

    fn implementation(&mut self, source: &Source, item: &ItemImpl) {
        let Some(owner) = simple_type(&item.self_ty) else {
            return;
        };
        let owner_key = (source.package.clone(), owner.clone());
        if item.trait_.is_some() {
            self.trait_owners.insert(owner_key);
            return;
        }
        for member in &item.items {
            let ImplItem::Fn(method) = member else {
                continue;
            };
            let record = Method {
                name: method.sig.ident.to_string(),
                signature: signature(&method.sig),
                location: source.location(method.span()),
            };
            self.methods
                .entry((source.package.clone(), owner.clone(), record.name.clone()))
                .or_default()
                .push(record);
        }
        let Some(wrapper) = self.wrappers.get_mut(&owner_key) else {
            return;
        };
        for member in &item.items {
            let ImplItem::Fn(method) = member else {
                wrapper.invalid = true;
                continue;
            };
            if boundary_attrs(&method.attrs) {
                wrapper.invalid = true;
                continue;
            }
            if let Some(forwarder) = forwarder(source, method, &wrapper.field) {
                wrapper.methods.push(forwarder);
            } else if !constructor(method, &wrapper.field, &wrapper.inner) {
                wrapper.invalid = true;
            }
        }
    }
}

struct Wrapper {
    package: String,
    name: String,
    inner: String,
    field: Member,
    location: crate::Location,
    methods: Vec<Forwarder>,
    invalid: bool,
}

impl Wrapper {
    fn finding(&self, database: &Database) -> Option<Finding> {
        let key = (self.package.clone(), self.name.clone());
        if self.invalid
            || self.methods.len() < MINIMUM_FORWARDERS
            || database.trait_owners.contains(&key)
            || database.ambiguous.contains(&key)
            || self.inner == self.name
        {
            return None;
        }

        let mut inner_methods = Vec::new();
        for forwarder in &self.methods {
            let matches = database
                .methods
                .get(&(self.package.clone(), self.inner.clone(), forwarder.name.clone()))?;
            if matches.len() != 1 || matches[0].signature != forwarder.signature {
                return None;
            }
            inner_methods.push(matches[0].clone());
        }

        let mut finding = Finding::warning(ID, self.name.clone(), self.location.clone());
        finding.message = format!(
            "`{}` only wraps local `{}` and forwards {} methods with identical names and signatures",
            self.name,
            self.inner,
            self.methods.len()
        );
        finding.help = "use the inner entity directly unless the wrapper owns a real invariant, translation, synchronization, instrumentation, adapter boundary, or compatibility contract".into();
        finding.related = self
            .methods
            .iter()
            .map(|method| Related {
                label: format!("transparent forwarder `{}`", method.name),
                location: method.location.clone(),
            })
            .chain(inner_methods.iter().map(|method| Related {
                label: format!("identical inner method `{}::{}`", self.inner, method.name),
                location: method.location.clone(),
            }))
            .collect();
        let mut review = Review::error();
        review.metadata = vec![
            ("Category".into(), "pure delegation wrapper".into()),
            ("Inner type".into(), self.inner.clone()),
            (
                "Exact forwarders".into(),
                self.methods
                    .iter()
                    .map(|method| method.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            ("Trait implementations".into(), "none".into()),
            ("Invariant or translation logic".into(), "none found".into()),
        ];
        review.questions = vec![
            "Does this wrapper own a non-syntactic boundary or compatibility promise?".into(),
            "Can callers use the inner entity without losing validation, state, or observability?".into(),
        ];
        finding.review = Some(review);
        Some(finding)
    }
}

#[derive(Clone)]
struct Method {
    name: String,
    signature: String,
    location: crate::Location,
}

struct Forwarder {
    name: String,
    signature: String,
    location: crate::Location,
}

fn forwarder(source: &Source, method: &syn::ImplItemFn, field: &Member) -> Option<Forwarder> {
    let [Stmt::Expr(expression, _)] = method.block.stmts.as_slice() else {
        return None;
    };
    let Expr::MethodCall(call) = strip(expression) else {
        return None;
    };
    if call.method != method.sig.ident || !self_field(&call.receiver, field) {
        return None;
    }
    let parameters = parameter_names(&method.sig)?;
    if call.args.len() != parameters.len()
        || !call
            .args
            .iter()
            .zip(parameters.iter())
            .all(|(argument, parameter)| path_name(argument).as_deref() == Some(parameter))
    {
        return None;
    }
    Some(Forwarder {
        name: method.sig.ident.to_string(),
        signature: signature(&method.sig),
        location: source.location(method.span()),
    })
}

fn constructor(method: &syn::ImplItemFn, field: &Member, inner: &str) -> bool {
    if method.sig.receiver().is_some() {
        return false;
    }
    let Some(parameters) = parameter_names(&method.sig) else {
        return false;
    };
    if parameters.len() != 1 {
        return false;
    }
    let Some(FnArg::Typed(argument)) = method.sig.inputs.first() else {
        return false;
    };
    if simple_type(&argument.ty).as_deref() != Some(inner) {
        return false;
    }
    let [Stmt::Expr(expression, _)] = method.block.stmts.as_slice() else {
        return false;
    };
    let Expr::Struct(structure) = strip(expression) else {
        return false;
    };
    if !structure.path.is_ident("Self") || structure.fields.len() != 1 || structure.rest.is_some() {
        return false;
    }
    let value = &structure.fields[0];
    value.member == *field && path_name(&value.expr).as_deref() == Some(parameters[0].as_str())
}

fn signature(signature: &syn::Signature) -> String {
    let receiver = signature
        .receiver()
        .map(|receiver| receiver.to_token_stream().to_string())
        .unwrap_or_default();
    let inputs = signature
        .inputs
        .iter()
        .filter_map(|argument| match argument {
            FnArg::Receiver(_) => None,
            FnArg::Typed(argument) => Some(argument.ty.to_token_stream().to_string()),
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{}|{}|{}|{}|{}",
        receiver,
        inputs,
        signature.output.to_token_stream(),
        signature.asyncness.is_some(),
        signature.unsafety.is_some(),
    )
}

fn parameter_names(signature: &syn::Signature) -> Option<Vec<String>> {
    signature
        .inputs
        .iter()
        .filter_map(|argument| match argument {
            FnArg::Receiver(_) => None,
            FnArg::Typed(argument) => Some(match argument.pat.as_ref() {
                Pat::Ident(ident) if ident.by_ref.is_none() && ident.subpat.is_none() => Some(ident.ident.to_string()),
                _ => None,
            }),
        })
        .collect()
}

fn self_field(expression: &Expr, expected: &Member) -> bool {
    let Expr::Field(field) = strip(expression) else {
        return false;
    };
    let Expr::Path(base) = strip(&field.base) else {
        return false;
    };
    base.path.is_ident("self") && field.member == *expected
}

fn path_name(expression: &Expr) -> Option<String> {
    let Expr::Path(path) = strip(expression) else {
        return None;
    };
    path.path.get_ident().map(std::string::ToString::to_string)
}

fn strip(expression: &Expr) -> &Expr {
    match expression {
        Expr::Group(group) => strip(&group.expr),
        Expr::Paren(paren) => strip(&paren.expr),
        _ => expression,
    }
}

fn simple_type(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else {
        return None;
    };
    (path.qself.is_none()
        && path.path.segments.len() == 1
        && matches!(path.path.segments[0].arguments, syn::PathArguments::None))
    .then(|| path.path.segments[0].ident.to_string())
}

fn boundary_attrs(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        if !attribute.path().is_ident("derive") {
            return true;
        }
        let Ok(derives) =
            attribute.parse_args_with(syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated)
        else {
            return true;
        };
        derives.iter().any(|derive| {
            !derive.segments.last().is_some_and(|segment| {
                matches!(
                    segment.ident.to_string().as_str(),
                    "Debug" | "Clone" | "Copy" | "Eq" | "PartialEq" | "Ord" | "PartialOrd" | "Hash" | "Default"
                )
            })
        })
    })
}

fn public(visibility: &Visibility) -> bool {
    matches!(visibility, Visibility::Public(_))
}
