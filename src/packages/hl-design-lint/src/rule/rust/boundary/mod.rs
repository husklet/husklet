use std::{collections::BTreeMap, path::Path};

use syn::{Attribute, Expr, ItemMod, Lit, Meta, spanned::Spanned};

use crate::{
    Result,
    model::{Finding, Review, Severity},
    rule::Rule,
    source::{Source, Workspace, requires_test},
};

#[cfg(test)]
#[path = "test.rs"]
mod tests;

/// Rejects explicit module paths that flatten several child domains into one namespace.
pub struct PathModules;

impl Rule for PathModules {
    fn id(&self) -> &'static str {
        "path-module-flattening"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        Ok(workspace
            .sources()
            .iter()
            .filter_map(|source| inspect(self.id(), source))
            .collect())
    }
}

fn inspect(rule: &'static str, source: &Source) -> Option<Finding> {
    let modules = source
        .syntax
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(module) if !allowed_wiring(&module.attrs) => explicit_path(module),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut domains = BTreeMap::<String, Vec<(&ItemMod, String)>>::new();
    for (module, path) in modules {
        let domain = Path::new(&path).components().next()?.as_os_str().to_str()?.to_owned();
        if Path::new(&path).components().count() > 1 {
            domains.entry(domain).or_default().push((module, path));
        }
    }
    if domains.len() < 2 {
        return None;
    }

    let first = domains.values().next()?.first()?.0;
    let names = domains.keys().cloned().collect::<Vec<_>>().join(", ");
    let declarations = domains.values().map(Vec::len).sum::<usize>();
    let mut finding = Finding::error(rule, source.path.display().to_string(), source.location(first.span()));
    finding.message = format!(
        "{declarations} explicit module paths flatten {} child domains into one namespace: {names}",
        domains.len()
    );
    finding.help = "declare noun-owned modules through their natural mod.rs or <noun>.rs boundary; re-export only the deliberately public surface".into();
    let mut review = Review::error();
    review.metadata = vec![
        ("child domains".into(), names),
        ("path declarations".into(), declarations.to_string()),
    ];
    review.questions = vec![
        "Which noun owns each injected capability and its state lifecycle?".into(),
        "Are internal representations being re-exported across that boundary?".into(),
    ];
    finding.review = Some(review);
    Some(finding)
}

fn explicit_path(module: &ItemMod) -> Option<(&ItemMod, String)> {
    module.attrs.iter().find_map(|attribute| {
        if !attribute.path().is_ident("path") {
            return None;
        }
        let Meta::NameValue(value) = &attribute.meta else {
            return None;
        };
        let Expr::Lit(value) = &value.value else {
            return None;
        };
        let Lit::Str(path) = &value.lit else {
            return None;
        };
        Some((module, path.value()))
    })
}

fn allowed_wiring(attributes: &[Attribute]) -> bool {
    requires_test(attributes) || attributes.iter().any(platform_cfg)
}

fn platform_cfg(attribute: &Attribute) -> bool {
    attribute.path().is_ident("cfg")
        && [
            "target_arch",
            "target_os",
            "target_family",
            "target_env",
            "target_vendor",
        ]
        .iter()
        .any(|key| attribute.meta.to_token_stream().to_string().contains(key))
}

use quote::ToTokens;
