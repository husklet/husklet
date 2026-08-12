use std::collections::BTreeSet;

use syn::{Expr, GenericArgument, Item, ItemFn, PathArguments, ReturnType, Type};

use crate::{
    Result,
    model::{Finding, Severity},
    rule::Rule,
    source::{Source, Workspace, requires_test},
};

#[cfg(test)]
#[path = "test.rs"]
mod tests;

/// Requires an unambiguously owned factory to live on the concrete type it creates.
pub struct Ownership;

impl Rule for Ownership {
    fn id(&self) -> &'static str {
        "detached-constructor"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for source in workspace.production() {
            let declared = declarations(&source.syntax.items);
            inspect_items(self.id(), source, &source.syntax.items, &declared, &mut findings);
        }
        Ok(findings)
    }
}

fn declarations(items: &[Item]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for item in items {
        match item {
            Item::Struct(item) => {
                names.insert(item.ident.to_string());
            }
            Item::Enum(item) => {
                names.insert(item.ident.to_string());
            }
            Item::Mod(module) => {
                if let Some((_, nested)) = &module.content {
                    names.extend(declarations(nested));
                }
            }
            _ => {}
        }
    }
    names
}

fn inspect_items(
    rule: &'static str,
    source: &Source,
    items: &[Item],
    declared: &BTreeSet<String>,
    findings: &mut Vec<Finding>,
) {
    for item in items {
        match item {
            Item::Fn(function) if !requires_test(&function.attrs) => {
                inspect_function(rule, source, function, declared, findings);
            }
            Item::Mod(module) if !requires_test(&module.attrs) => {
                if let Some((_, nested)) = &module.content {
                    inspect_items(rule, source, nested, declared, findings);
                }
            }
            // Implementation methods already have an owner. Do not descend into impls, traits,
            // function bodies, or foreign blocks.
            _ => {}
        }
    }
}

fn inspect_function(
    rule: &'static str,
    source: &Source,
    function: &ItemFn,
    declared: &BTreeSet<String>,
    findings: &mut Vec<Finding>,
) {
    if !function.sig.generics.params.is_empty() {
        return;
    }
    let Some(owner) = returned_owner(&function.sig.output) else {
        return;
    };
    if !declared.contains(&owner) || !constructs(&function.block, &owner) {
        return;
    }

    let name = function.sig.ident.to_string();
    let mut finding = Finding::error(rule, &name, source.location(function.sig.ident.span()));
    finding.message =
        format!("free function `{name}` constructs and returns `{owner}`, so `{owner}` is its natural owner");
    finding.help = format!("move it into `impl {owner}` and call it as `{owner}::{name}(...)`");
    findings.push(finding);
}

fn returned_owner(output: &ReturnType) -> Option<String> {
    let ReturnType::Type(_, ty) = output else {
        return None;
    };
    concrete_owner(ty)
}

fn concrete_owner(ty: &Type) -> Option<String> {
    let Type::Path(ty) = ty else {
        return None;
    };
    if ty.qself.is_some() {
        return None;
    }
    let segment = ty.path.segments.last()?;
    if matches!(segment.ident.to_string().as_str(), "Option" | "Result") {
        let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
            return None;
        };
        let GenericArgument::Type(inner) = arguments.args.first()? else {
            return None;
        };
        return concrete_owner(inner);
    }
    if !matches!(segment.arguments, PathArguments::None) || !local_path(&ty.path) {
        return None;
    }
    Some(segment.ident.to_string())
}

fn local_path(path: &syn::Path) -> bool {
    path.segments.len() == 1
        || matches!(
            path.segments
                .first()
                .map(|segment| segment.ident.to_string())
                .as_deref(),
            Some("crate" | "self" | "super")
        )
}

fn constructs(block: &syn::Block, owner: &str) -> bool {
    block.stmts.last().is_some_and(|statement| match statement {
        syn::Stmt::Expr(expression, _) => constructed_expression(expression, owner),
        _ => false,
    })
}

fn constructed_expression(expression: &Expr, owner: &str) -> bool {
    match expression {
        Expr::Struct(value) => path_owner(&value.path, owner),
        Expr::Path(value) => path_owner(&value.path, owner),
        Expr::Call(value) => {
            let Expr::Path(function) = value.func.as_ref() else {
                return false;
            };
            let segments = &function.path.segments;
            if segments.len() == 1 && segments[0].ident == owner {
                return true;
            }
            if segments.len() == 1 && matches!(segments[0].ident.to_string().as_str(), "Ok" | "Some") {
                return value.args.len() == 1 && constructed_expression(&value.args[0], owner);
            }
            same_owner_factory(&function.path, owner)
        }
        Expr::Block(value) => constructs(&value.block, owner),
        Expr::Paren(value) => constructed_expression(&value.expr, owner),
        Expr::Group(value) => constructed_expression(&value.expr, owner),
        Expr::Try(value) => constructed_expression(&value.expr, owner),
        _ => false,
    }
}

fn path_owner(path: &syn::Path, owner: &str) -> bool {
    local_path(path) && path.segments.last().is_some_and(|segment| segment.ident == owner)
}

fn same_owner_factory(path: &syn::Path, owner: &str) -> bool {
    if path.segments.len() != 2 || path.segments[0].ident != owner {
        return false;
    }
    !matches!(
        path.segments[1].ident.to_string().as_str(),
        "from" | "try_from" | "into" | "try_into" | "as_ref" | "as_mut"
    )
}
