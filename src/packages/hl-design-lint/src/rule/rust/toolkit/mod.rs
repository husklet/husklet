use std::collections::BTreeSet;

use quote::ToTokens;
use syn::{
    Fields, ImplItem, Item, ItemEnum, ItemImpl, ItemStruct, ItemTrait, Path, ReturnType, TraitItem, Type, UseTree,
    Visibility, spanned::Spanned, visit::Visit,
};

use crate::{
    Result,
    model::{Finding, Review, Severity},
    source::{Source, Workspace},
};

use crate::rule::Rule;

const TOOLKITS: &[&str] = &["gtk", "gdk", "glib", "vte4"];

mod aliases;
mod reachability;

use aliases::Aliases;
use reachability::{impl_type_name, item_name, public_files, reexported_items};

/// Rejects native toolkit types in `hl-gui`'s externally reachable API.
pub struct GuiToolkitLeakage;

impl Rule for GuiToolkitLeakage {
    fn id(&self) -> &'static str {
        "gui-toolkit-type-leakage"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let sources = workspace
            .production()
            .filter(|source| source.package == "hl-gui")
            .collect::<Vec<_>>();
        if sources.is_empty() {
            return Ok(Vec::new());
        }

        let public_files = public_files(&sources);
        let reexports = reexported_items(&sources, &public_files);
        let mut findings = Vec::new();
        for source in &sources {
            if !public_files.contains(&source.path) {
                continue;
            }
            inspect_public_source(self.id(), source, &mut findings);
        }
        for source in sources {
            let Some(names) = reexports.get(&source.path) else {
                continue;
            };
            if public_files.contains(&source.path) {
                continue;
            }
            inspect_reexports(self.id(), source, names, &mut findings);
        }
        Ok(findings)
    }
}

fn inspect_public_source(rule: &'static str, source: &Source, findings: &mut Vec<Finding>) {
    let aliases = Aliases::from_source(source);
    Visitor {
        rule,
        source,
        aliases: &aliases,
        public_context: true,
        public_types: public_type_names(source),
        findings,
    }
    .visit_file(&source.syntax);
}

fn inspect_reexports(rule: &'static str, source: &Source, names: &BTreeSet<String>, findings: &mut Vec<Finding>) {
    let aliases = Aliases::from_source(source);
    let mut visitor = Visitor {
        rule,
        source,
        aliases: &aliases,
        public_context: true,
        public_types: names.clone(),
        findings,
    };
    for item in source.syntax.items.iter().filter(|item| exported(item, names)) {
        visitor.visit_item(item);
    }
}

fn exported(item: &Item, names: &BTreeSet<String>) -> bool {
    item_name(item).is_some_and(|name| names.contains(&name))
        || match item {
            Item::Impl(item) => impl_type_name(item).is_some_and(|name| names.contains(&name)),
            _ => false,
        }
}

struct Visitor<'a> {
    rule: &'static str,
    source: &'a Source,
    aliases: &'a Aliases,
    public_context: bool,
    public_types: BTreeSet<String>,
    findings: &'a mut Vec<Finding>,
}

impl Visitor<'_> {
    // The finding owns the subject it reports.
    #[allow(clippy::needless_pass_by_value)]
    fn inspect(&mut self, subject: String, span: proc_macro2::Span, types: impl IntoIterator<Item = Type>) {
        let mut leaks = BTreeSet::new();
        for ty in types {
            TypeLeaks {
                aliases: self.aliases,
                leaks: &mut leaks,
            }
            .visit_type(&ty);
        }
        if leaks.is_empty() {
            return;
        }
        let leaked = leaks.into_iter().collect::<Vec<_>>().join(", ");
        let mut finding = Finding::error(self.rule, subject.clone(), self.source.location(span));
        finding.message = format!("externally reachable `{subject}` exposes native GUI toolkit type(s): {leaked}");
        finding.help = "replace native types with hl-gui-owned state, events, component handles, or errors; keep toolkit conversion and widget access inside the adapter".into();
        let mut review = Review::error();
        review.metadata = vec![
            ("package".into(), self.source.package.clone()),
            ("signature".into(), self.source.excerpt(span)),
            ("leaked toolkit types".into(), leaked),
        ];
        review.questions = vec![
            "What toolkit-neutral value or intent is crossing this boundary?".into(),
            "Can the native conversion remain entirely inside the selected adapter?".into(),
        ];
        finding.review = Some(review);
        self.findings.push(finding);
    }

    fn public(&self, visibility: &Visibility) -> bool {
        self.public_context && matches!(visibility, Visibility::Public(_))
    }
}

impl<'ast> Visit<'ast> for Visitor<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        let previous = self.public_context;
        self.public_context = previous && matches!(item.vis, Visibility::Public(_));
        if let Some((_, items)) = &item.content {
            for item in items {
                self.visit_item(item);
            }
        }
        self.public_context = previous;
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if self.public(&item.vis) {
            self.inspect(item.sig.ident.to_string(), item.sig.span(), signature_types(&item.sig));
        }
    }

    fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
        if self.public(&item.vis) {
            let mut types = generic_types(&item.generics);
            types.push((*item.ty).clone());
            self.inspect(item.ident.to_string(), item.span(), types);
        }
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        if !self.public(&item.vis) {
            return;
        }
        let mut leaks = BTreeSet::new();
        collect_toolkit_uses(&item.tree, None, self.aliases, &mut leaks);
        if !leaks.is_empty() {
            let leaked = leaks.into_iter().collect::<Vec<_>>().join(", ");
            let subject = item.tree.to_token_stream().to_string();
            let mut finding = Finding::error(self.rule, subject.clone(), self.source.location(item.span()));
            finding.message = format!("externally reachable re-export `{subject}` exposes {leaked}");
            finding.help =
                "export an hl-gui-owned type and keep the native toolkit import private to the adapter".into();
            let mut review = Review::error();
            review.metadata = vec![
                ("package".into(), self.source.package.clone()),
                ("signature".into(), self.source.excerpt(item.span())),
                ("leaked toolkit types".into(), leaked),
            ];
            finding.review = Some(review);
            self.findings.push(finding);
        }
    }

    fn visit_item_const(&mut self, item: &'ast syn::ItemConst) {
        if self.public(&item.vis) {
            self.inspect(item.ident.to_string(), item.span(), [(*item.ty).clone()]);
        }
    }

    fn visit_item_static(&mut self, item: &'ast syn::ItemStatic) {
        if self.public(&item.vis) {
            self.inspect(item.ident.to_string(), item.span(), [(*item.ty).clone()]);
        }
    }

    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        if !self.public(&item.vis) {
            return;
        }
        let mut types = generic_types(&item.generics);
        types.extend(visible_field_types(&item.fields));
        self.inspect(item.ident.to_string(), item.span(), types);
    }

    fn visit_item_enum(&mut self, item: &'ast ItemEnum) {
        if !self.public(&item.vis) {
            return;
        }
        let mut types = generic_types(&item.generics);
        types.extend(
            item.variants
                .iter()
                .flat_map(|variant| variant.fields.iter())
                .map(|field| field.ty.clone()),
        );
        self.inspect(item.ident.to_string(), item.span(), types);
    }

    fn visit_item_trait(&mut self, item: &'ast ItemTrait) {
        if !self.public(&item.vis) {
            return;
        }
        let mut declaration_types = generic_types(&item.generics);
        declaration_types.extend(item.supertraits.iter().filter_map(bound_type));
        self.inspect(item.ident.to_string(), item.span(), declaration_types);
        for member in &item.items {
            match member {
                TraitItem::Fn(method) => self.inspect(
                    format!("{}::{}", item.ident, method.sig.ident),
                    method.sig.span(),
                    signature_types(&method.sig),
                ),
                TraitItem::Type(ty) => {
                    let types = ty
                        .bounds
                        .iter()
                        .filter_map(bound_type)
                        .chain(ty.default.as_ref().map(|(_, ty)| ty.clone()))
                        .collect::<Vec<_>>();
                    self.inspect(format!("{}::{}", item.ident, ty.ident), ty.span(), types);
                }
                TraitItem::Const(value) => self.inspect(
                    format!("{}::{}", item.ident, value.ident),
                    value.span(),
                    [value.ty.clone()],
                ),
                _ => {}
            }
        }
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        if !self.public_context || item.trait_.is_some() || !public_self_type(item, &self.public_types) {
            return;
        }
        let owner = item.self_ty.to_token_stream().to_string();
        for member in &item.items {
            if let ImplItem::Fn(method) = member
                && matches!(method.vis, Visibility::Public(_))
            {
                self.inspect(
                    format!("{owner}::{}", method.sig.ident),
                    method.sig.span(),
                    signature_types(&method.sig),
                );
            }
        }
    }
}

struct TypeLeaks<'a> {
    aliases: &'a Aliases,
    leaks: &'a mut BTreeSet<String>,
}

impl<'ast> Visit<'ast> for TypeLeaks<'_> {
    fn visit_path(&mut self, path: &'ast Path) {
        if let Some(toolkit) = self.aliases.toolkit(path) {
            self.leaks.insert(format!("{toolkit} ({})", path.to_token_stream()));
        }
        syn::visit::visit_path(self, path);
    }
}

fn signature_types(signature: &syn::Signature) -> Vec<Type> {
    let mut types = signature
        .inputs
        .iter()
        .filter_map(|argument| match argument {
            syn::FnArg::Typed(argument) => Some((*argument.ty).clone()),
            syn::FnArg::Receiver(_) => None,
        })
        .collect::<Vec<_>>();
    if let ReturnType::Type(_, ty) = &signature.output {
        types.push((**ty).clone());
    }
    types.extend(generic_types(&signature.generics));
    types
}

fn generic_types(generics: &syn::Generics) -> Vec<Type> {
    generics
        .params
        .iter()
        .flat_map(generic_parameter_types)
        .chain(
            generics
                .where_clause
                .iter()
                .flat_map(|clause| &clause.predicates)
                .flat_map(where_predicate_types),
        )
        .collect()
}

fn generic_parameter_types(parameter: &syn::GenericParam) -> Vec<Type> {
    let syn::GenericParam::Type(parameter) = parameter else {
        return Vec::new();
    };
    parameter
        .bounds
        .iter()
        .filter_map(bound_type)
        .chain(parameter.default.iter().cloned())
        .collect()
}

fn where_predicate_types(predicate: &syn::WherePredicate) -> Vec<Type> {
    let syn::WherePredicate::Type(predicate) = predicate else {
        return Vec::new();
    };
    std::iter::once(predicate.bounded_ty.clone())
        .chain(predicate.bounds.iter().filter_map(bound_type))
        .collect()
}

fn collect_toolkit_uses(tree: &UseTree, prefix: Option<String>, aliases: &Aliases, leaks: &mut BTreeSet<String>) {
    match tree {
        UseTree::Path(path) => {
            let next = prefix.map_or_else(|| path.ident.to_string(), |prefix| format!("{prefix}::{}", path.ident));
            collect_toolkit_uses(&path.tree, Some(next), aliases, leaks);
        }
        UseTree::Name(name) => {
            record_toolkit_use(prefix, name.ident.to_string(), aliases, leaks);
        }
        UseTree::Rename(rename) => {
            record_toolkit_use(prefix, rename.ident.to_string(), aliases, leaks);
        }
        UseTree::Group(group) => {
            for tree in &group.items {
                collect_toolkit_uses(tree, prefix.clone(), aliases, leaks);
            }
        }
        UseTree::Glob(_) => record_toolkit_use(prefix, "*".into(), aliases, leaks),
    }
}

// The finding owns the subject it reports.
#[allow(clippy::needless_pass_by_value)]
fn record_toolkit_use(prefix: Option<String>, leaf: String, aliases: &Aliases, leaks: &mut BTreeSet<String>) {
    let path = prefix.map_or_else(|| leaf.clone(), |prefix| format!("{prefix}::{leaf}"));
    let root = path.split("::").next().unwrap_or_default();
    if let Some(toolkit) = aliases.crate_toolkit(root) {
        leaks.insert(format!("{toolkit} ({path})"));
    }
}

fn bound_type(bound: &syn::TypeParamBound) -> Option<Type> {
    match bound {
        syn::TypeParamBound::Trait(bound) => Some(Type::Path(syn::TypePath {
            qself: None,
            path: bound.path.clone(),
        })),
        _ => None,
    }
}

fn visible_field_types(fields: &Fields) -> Vec<Type> {
    fields
        .iter()
        .filter(|field| matches!(field.vis, Visibility::Public(_)))
        .map(|field| field.ty.clone())
        .collect()
}

fn public_type_names(source: &Source) -> BTreeSet<String> {
    source
        .syntax
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Struct(item) if matches!(item.vis, Visibility::Public(_)) => Some(item.ident.to_string()),
            Item::Enum(item) if matches!(item.vis, Visibility::Public(_)) => Some(item.ident.to_string()),
            Item::Union(item) if matches!(item.vis, Visibility::Public(_)) => Some(item.ident.to_string()),
            _ => None,
        })
        .collect()
}

fn public_self_type(item: &ItemImpl, public_types: &BTreeSet<String>) -> bool {
    match item.self_ty.as_ref() {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .is_some_and(|segment| public_types.contains(&segment.ident.to_string())),
        Type::Group(group) => type_is_public(&group.elem, public_types),
        Type::Paren(paren) => type_is_public(&paren.elem, public_types),
        _ => false,
    }
}

fn type_is_public(ty: &Type, public_types: &BTreeSet<String>) -> bool {
    match ty {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .is_some_and(|segment| public_types.contains(&segment.ident.to_string())),
        Type::Group(group) => type_is_public(&group.elem, public_types),
        Type::Paren(paren) => type_is_public(&paren.elem, public_types),
        _ => false,
    }
}

#[cfg(test)]
#[path = "test.rs"]
mod tests;
