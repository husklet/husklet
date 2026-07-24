use std::collections::{BTreeMap, BTreeSet};

use quote::ToTokens;
use syn::{
    spanned::Spanned, Expr, FnArg, ImplItem, Item, ItemImpl, ItemStruct, Member, Pat, Stmt,
    Visibility,
};

use crate::{
    model::{Finding, Related, Review, Severity},
    source::{Source, Workspace},
    Result,
};

use super::Rule;

/// Detects accessors that duplicate access callers already have.
pub struct AccessorBloat;

impl Rule for AccessorBloat {
    fn id(&self) -> &'static str {
        "redundant-accessor"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let definitions = Definitions::collect(workspace);
        let mut findings = Vec::new();
        for source in workspace.production() {
            inspect_source(source, &definitions, &mut findings);
        }
        Ok(findings)
    }
}

#[derive(Clone)]
struct Field {
    visibility: String,
    location: crate::Location,
}

#[derive(Clone)]
struct Structure {
    fields: BTreeMap<String, Field>,
    excluded: bool,
}

#[derive(Default)]
struct Definitions {
    structures: BTreeMap<(String, String), Structure>,
    ambiguous: BTreeSet<(String, String)>,
}

impl Definitions {
    fn collect(workspace: &Workspace) -> Self {
        let mut definitions = Self::default();
        for source in workspace.production() {
            for item in &source.syntax.items {
                let Item::Struct(item) = item else {
                    continue;
                };
                definitions.insert(source, item);
            }
        }
        definitions
    }

    fn insert(&mut self, source: &Source, item: &ItemStruct) {
        let key = (source.package.clone(), item.ident.to_string());
        if self.structures.contains_key(&key) {
            self.ambiguous.insert(key);
            return;
        }
        let fields = item
            .fields
            .iter()
            .filter_map(|field| {
                field.ident.as_ref().map(|name| {
                    (
                        name.to_string(),
                        Field {
                            visibility: visibility(&field.vis),
                            location: source.location(field.span()),
                        },
                    )
                })
            })
            .collect();
        self.structures.insert(
            key,
            Structure {
                fields,
                excluded: boundary_attributes(&item.attrs),
            },
        );
    }

    fn get(&self, package: &str, name: &str) -> Option<&Structure> {
        let key = (package.to_owned(), name.to_owned());
        (!self.ambiguous.contains(&key))
            .then(|| self.structures.get(&key))
            .flatten()
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Access {
    Get { field: String, shape: Shape },
    Set { field: String },
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Shape {
    Value,
    SharedReference,
    MutableReference,
    Clone,
}

struct Candidate<'a> {
    method: &'a syn::ImplItemFn,
    access: Access,
    contract: String,
}

fn inspect_source(source: &Source, definitions: &Definitions, findings: &mut Vec<Finding>) {
    for item in &source.syntax.items {
        let Item::Impl(item) = item else {
            continue;
        };
        inspect_impl(source, definitions, item, findings);
    }
}

fn inspect_impl(
    source: &Source,
    definitions: &Definitions,
    item: &ItemImpl,
    findings: &mut Vec<Finding>,
) {
    if item.trait_.is_some() || boundary_attributes(&item.attrs) {
        return;
    }
    let Some(owner) = type_name(&item.self_ty) else {
        return;
    };
    let Some(structure) = definitions.get(&source.package, &owner) else {
        return;
    };
    if structure.excluded {
        return;
    }

    let candidates = item
        .items
        .iter()
        .filter_map(|member| match member {
            ImplItem::Fn(method) => candidate(method),
            _ => None,
        })
        .collect::<Vec<_>>();

    for candidate in &candidates {
        let field_name = match &candidate.access {
            Access::Get { field, .. } | Access::Set { field } => field,
        };
        let Some(field) = structure.fields.get(field_name) else {
            continue;
        };
        if exposure_is_redundant(&field.visibility, &candidate.method.vis) {
            findings.push(exposed_finding(source, &owner, candidate, field));
        }
    }

    for (index, left) in candidates.iter().enumerate() {
        for right in &candidates[index + 1..] {
            if left.access == right.access
                && left.contract == right.contract
                && visibility(&left.method.vis) == visibility(&right.method.vis)
            {
                findings.push(duplicate_finding(source, &owner, left, right));
            }
        }
    }
}

fn candidate(method: &syn::ImplItemFn) -> Option<Candidate<'_>> {
    if boundary_attributes(&method.attrs) || method.sig.asyncness.is_some() {
        return None;
    }
    let expression = single_expression(&method.block.stmts)?;
    if let Some((field, shape)) = getter(expression) {
        return Some(Candidate {
            method,
            access: Access::Get { field, shape },
            contract: method.sig.output.to_token_stream().to_string(),
        });
    }
    setter(method, expression).map(|(field, contract)| Candidate {
        method,
        access: Access::Set { field },
        contract,
    })
}

fn single_expression(statements: &[Stmt]) -> Option<&Expr> {
    match statements {
        [Stmt::Expr(expression, _)] => Some(strip(expression)),
        _ => None,
    }
}

fn strip(expression: &Expr) -> &Expr {
    match expression {
        Expr::Group(group) => strip(&group.expr),
        Expr::Paren(paren) => strip(&paren.expr),
        _ => expression,
    }
}

fn getter(expression: &Expr) -> Option<(String, Shape)> {
    match strip(expression) {
        Expr::Field(field) => self_field(field).map(|name| (name, Shape::Value)),
        Expr::Reference(reference) => {
            let Expr::Field(field) = strip(&reference.expr) else {
                return None;
            };
            self_field(field).map(|name| {
                (
                    name,
                    if reference.mutability.is_some() {
                        Shape::MutableReference
                    } else {
                        Shape::SharedReference
                    },
                )
            })
        }
        Expr::MethodCall(call)
            if call.method == "clone"
                && call.args.is_empty()
                && matches!(strip(&call.receiver), Expr::Field(_)) =>
        {
            let Expr::Field(field) = strip(&call.receiver) else {
                unreachable!()
            };
            self_field(field).map(|name| (name, Shape::Clone))
        }
        _ => None,
    }
}

fn setter(method: &syn::ImplItemFn, expression: &Expr) -> Option<(String, String)> {
    let Expr::Assign(assign) = expression else {
        return None;
    };
    let Expr::Field(field) = strip(&assign.left) else {
        return None;
    };
    let field = self_field(field)?;
    let Expr::Path(value) = strip(&assign.right) else {
        return None;
    };
    let (argument, argument_type) =
        method
            .sig
            .inputs
            .iter()
            .find_map(|argument| match argument {
                FnArg::Typed(argument) => match argument.pat.as_ref() {
                    Pat::Ident(name) => Some((
                        name.ident.to_string(),
                        argument.ty.to_token_stream().to_string(),
                    )),
                    _ => None,
                },
                FnArg::Receiver(_) => None,
            })?;
    (method.sig.inputs.len() == 2
        && value.path.is_ident(&argument)
        && matches!(method.sig.output, syn::ReturnType::Default))
    .then_some((field, argument_type))
}

fn self_field(field: &syn::ExprField) -> Option<String> {
    let Expr::Path(base) = strip(&field.base) else {
        return None;
    };
    if !base.path.is_ident("self") {
        return None;
    }
    match &field.member {
        Member::Named(name) => Some(name.to_string()),
        Member::Unnamed(_) => None,
    }
}

fn exposed_finding(
    source: &Source,
    owner: &str,
    candidate: &Candidate<'_>,
    field: &Field,
) -> Finding {
    let method = candidate.method.sig.ident.to_string();
    let field_name = match &candidate.access {
        Access::Get { field, .. } | Access::Set { field } => field,
    };
    let mut finding = Finding::warning(
        "redundant-accessor",
        format!("{owner}::{method}"),
        source.location(candidate.method.span()),
    );
    finding.message = format!(
        "`{method}` only forwards `{owner}.{field_name}`, which callers can already access with equal or broader visibility"
    );
    finding.help = format!(
        "remove the zero-contract accessor or make `{field_name}` private when the method intentionally defines the stable API boundary"
    );
    finding.related.push(Related {
        label: "already exposed field".into(),
        location: field.location.clone(),
    });
    let mut review = Review::error();
    review.metadata = vec![
        ("Owner".into(), owner.into()),
        ("Method".into(), method),
        ("Field".into(), field_name.clone()),
        ("Field visibility".into(), field.visibility.clone()),
        (
            "Method visibility".into(),
            visibility(&candidate.method.vis),
        ),
        ("Forwarding shape".into(), format!("{:?}", candidate.access)),
    ];
    review.questions = vec![
        "Is direct field access already part of the same or a broader API surface?".into(),
        "Should the field become private because this accessor intentionally preserves representation or mutation policy?".into(),
        "Does compatibility require retaining this method, and is that contract documented explicitly?".into(),
    ];
    finding.review = Some(review);
    finding
}

fn duplicate_finding(
    source: &Source,
    owner: &str,
    left: &Candidate<'_>,
    right: &Candidate<'_>,
) -> Finding {
    let left_name = left.method.sig.ident.to_string();
    let right_name = right.method.sig.ident.to_string();
    let mut finding = Finding::warning(
        "redundant-accessor",
        format!("{owner}::{right_name}"),
        source.location(right.method.span()),
    );
    finding.message = format!(
        "`{owner}::{left_name}` and `{owner}::{right_name}` expose the identical field operation"
    );
    finding.help =
        "keep one canonical accessor unless a documented compatibility contract requires both"
            .into();
    finding.related.push(Related {
        label: "identical accessor".into(),
        location: source.location(left.method.span()),
    });
    let mut review = Review::error();
    review.metadata = vec![
        ("Owner".into(), owner.into()),
        ("First accessor".into(), left_name),
        ("Duplicate accessor".into(), right_name),
        ("Operation".into(), format!("{:?}", right.access)),
    ];
    review.questions = vec![
        "Do both names represent one contract rather than distinct domain vocabulary?".into(),
        "Is either alias required by a documented compatibility boundary?".into(),
    ];
    finding.review = Some(review);
    finding
}

fn exposure_is_redundant(field: &str, method: &Visibility) -> bool {
    field != "private" && (field == "pub" || field == visibility(method))
}

fn visibility(visibility: &Visibility) -> String {
    match visibility {
        Visibility::Inherited => "private".into(),
        _ => visibility.to_token_stream().to_string().replace(' ', ""),
    }
}

fn type_name(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(path) if path.qself.is_none() => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string()),
        syn::Type::Group(group) => type_name(&group.elem),
        syn::Type::Paren(paren) => type_name(&paren.elem),
        _ => None,
    }
}

fn boundary_attributes(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        let name = attribute
            .path()
            .segments
            .last()
            .map(|segment| segment.ident.to_string());
        matches!(
            name.as_deref(),
            Some(
                "deprecated"
                    | "serde"
                    | "repr"
                    | "no_mangle"
                    | "export_name"
                    | "link_name"
                    | "cfg"
                    | "cfg_attr"
            )
        ) || (attribute.path().is_ident("derive") && {
            let tokens = attribute.meta.to_token_stream().to_string();
            tokens.contains("Serialize") || tokens.contains("Deserialize")
        })
    })
}
