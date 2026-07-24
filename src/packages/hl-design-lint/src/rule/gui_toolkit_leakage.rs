use std::collections::{BTreeMap, BTreeSet};

use quote::ToTokens;
use syn::{
    spanned::Spanned, visit::Visit, Fields, ImplItem, Item, ItemEnum, ItemImpl, ItemStruct,
    ItemTrait, Path, ReturnType, TraitItem, Type, UseTree, Visibility,
};

use crate::{
    model::{Finding, Review, Severity},
    source::{Source, Workspace},
    Result,
};

use super::Rule;

const TOOLKITS: &[&str] = &["gtk", "gdk", "glib", "vte4"];

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
            let aliases = Aliases::from_source(source);
            Visitor {
                rule: self.id(),
                source,
                aliases: &aliases,
                public_context: true,
                public_types: public_type_names(source),
                findings: &mut findings,
            }
            .visit_file(&source.syntax);
        }
        for source in sources {
            let Some(names) = reexports.get(&source.path) else {
                continue;
            };
            if public_files.contains(&source.path) {
                continue;
            }
            let aliases = Aliases::from_source(source);
            let mut visitor = Visitor {
                rule: self.id(),
                source,
                aliases: &aliases,
                public_context: true,
                public_types: names.clone(),
                findings: &mut findings,
            };
            for item in &source.syntax.items {
                let selected = item_name(item).is_some_and(|name| names.contains(&name));
                let selected_impl = match item {
                    Item::Impl(item) => {
                        impl_type_name(item).is_some_and(|name| names.contains(&name))
                    }
                    _ => false,
                };
                if selected || selected_impl {
                    visitor.visit_item(item);
                }
            }
        }
        Ok(findings)
    }
}

#[derive(Default)]
struct Aliases {
    crates: BTreeMap<String, String>,
    types: BTreeMap<String, String>,
}

impl Aliases {
    fn from_source(source: &Source) -> Self {
        let mut aliases = Self::default();
        let local_modules = source
            .syntax
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Mod(module) => Some(module.ident.to_string()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        for toolkit in TOOLKITS {
            if !local_modules.contains(*toolkit) {
                aliases.crates.insert((*toolkit).into(), (*toolkit).into());
            }
        }
        for item in &source.syntax.items {
            match item {
                Item::Use(item) => collect_use(&item.tree, None, &mut aliases),
                Item::ExternCrate(item) => {
                    let original = item.ident.to_string();
                    if TOOLKITS.contains(&original.as_str()) {
                        aliases.crates.insert(
                            item.rename
                                .as_ref()
                                .map_or_else(|| original.clone(), |(_, name)| name.to_string()),
                            original,
                        );
                    }
                }
                _ => {}
            }
        }
        aliases
    }

    fn toolkit(&self, path: &Path) -> Option<String> {
        let first = path.segments.first()?.ident.to_string();
        if let Some(toolkit) = self.crates.get(&first) {
            return Some(toolkit.clone());
        }
        self.types.get(&first).cloned()
    }
}

fn collect_use(tree: &UseTree, prefix: Option<String>, aliases: &mut Aliases) {
    match tree {
        UseTree::Path(path) => {
            let next = match prefix {
                Some(prefix) => format!("{prefix}::{}", path.ident),
                None => path.ident.to_string(),
            };
            collect_use(&path.tree, Some(next), aliases);
        }
        UseTree::Name(name) => record_use(prefix, name.ident.to_string(), aliases),
        UseTree::Rename(rename) => {
            if let Some(prefix) = prefix {
                record_use(Some(prefix), rename.rename.to_string(), aliases);
            } else {
                let original = rename.ident.to_string();
                if TOOLKITS.contains(&original.as_str()) {
                    aliases.crates.insert(rename.rename.to_string(), original);
                }
            }
        }
        UseTree::Group(group) => {
            for item in &group.items {
                collect_use(item, prefix.clone(), aliases);
            }
        }
        UseTree::Glob(_) => {}
    }
}

fn record_use(prefix: Option<String>, local: String, aliases: &mut Aliases) {
    let Some(path) = prefix else {
        return;
    };
    let root = path.split("::").next().unwrap_or_default();
    if !TOOLKITS.contains(&root) {
        return;
    }
    if path == root {
        aliases.crates.insert(local, root.into());
    } else {
        aliases.types.insert(local, root.into());
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
    fn inspect(
        &mut self,
        subject: String,
        span: proc_macro2::Span,
        types: impl IntoIterator<Item = Type>,
    ) {
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
        finding.message = format!(
            "externally reachable `{subject}` exposes native GUI toolkit type(s): {leaked}"
        );
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
            self.inspect(
                item.sig.ident.to_string(),
                item.sig.span(),
                signature_types(&item.sig),
            );
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
            let mut finding = Finding::error(
                self.rule,
                subject.clone(),
                self.source.location(item.span()),
            );
            finding.message =
                format!("externally reachable re-export `{subject}` exposes {leaked}");
            finding.help = "export an hl-gui-owned type and keep the native toolkit import private to the adapter".into();
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
        if !self.public_context
            || item.trait_.is_some()
            || !public_self_type(item, &self.public_types)
        {
            return;
        }
        let owner = item.self_ty.to_token_stream().to_string();
        for member in &item.items {
            if let ImplItem::Fn(method) = member {
                if matches!(method.vis, Visibility::Public(_)) {
                    self.inspect(
                        format!("{owner}::{}", method.sig.ident),
                        method.sig.span(),
                        signature_types(&method.sig),
                    );
                }
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
            self.leaks
                .insert(format!("{toolkit} ({})", path.to_token_stream()));
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
    let mut types = Vec::new();
    for parameter in &generics.params {
        if let syn::GenericParam::Type(parameter) = parameter {
            types.extend(parameter.bounds.iter().filter_map(bound_type));
            if let Some(default) = &parameter.default {
                types.push(default.clone());
            }
        }
    }
    for predicate in generics
        .where_clause
        .iter()
        .flat_map(|clause| &clause.predicates)
    {
        if let syn::WherePredicate::Type(predicate) = predicate {
            types.push(predicate.bounded_ty.clone());
            types.extend(predicate.bounds.iter().filter_map(bound_type));
        }
    }
    types
}

fn collect_toolkit_uses(
    tree: &UseTree,
    prefix: Option<String>,
    aliases: &Aliases,
    leaks: &mut BTreeSet<String>,
) {
    match tree {
        UseTree::Path(path) => {
            let next = prefix.map_or_else(
                || path.ident.to_string(),
                |prefix| format!("{prefix}::{}", path.ident),
            );
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

fn record_toolkit_use(
    prefix: Option<String>,
    leaf: String,
    aliases: &Aliases,
    leaks: &mut BTreeSet<String>,
) {
    let path = prefix.map_or_else(|| leaf.clone(), |prefix| format!("{prefix}::{leaf}"));
    let root = path.split("::").next().unwrap_or_default();
    if let Some(toolkit) = aliases.crates.get(root) {
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
            Item::Struct(item) if matches!(item.vis, Visibility::Public(_)) => {
                Some(item.ident.to_string())
            }
            Item::Enum(item) if matches!(item.vis, Visibility::Public(_)) => {
                Some(item.ident.to_string())
            }
            Item::Union(item) if matches!(item.vis, Visibility::Public(_)) => {
                Some(item.ident.to_string())
            }
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
        Type::Group(group) => public_self_type_for(&group.elem, public_types),
        Type::Paren(paren) => public_self_type_for(&paren.elem, public_types),
        _ => false,
    }
}

fn public_self_type_for(ty: &Type, public_types: &BTreeSet<String>) -> bool {
    match ty {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .is_some_and(|segment| public_types.contains(&segment.ident.to_string())),
        Type::Group(group) => public_self_type_for(&group.elem, public_types),
        Type::Paren(paren) => public_self_type_for(&paren.elem, public_types),
        _ => false,
    }
}

fn public_files(sources: &[&Source]) -> BTreeSet<std::path::PathBuf> {
    let mut public = sources
        .iter()
        .filter(|source| {
            matches!(
                source.path.file_name().and_then(|name| name.to_str()),
                Some("lib.rs")
            )
        })
        .map(|source| source.path.clone())
        .collect::<BTreeSet<_>>();
    loop {
        let mut changed = false;
        for source in sources {
            if !public.contains(&source.path) {
                continue;
            }
            for item in &source.syntax.items {
                let Item::Mod(module) = item else { continue };
                if !matches!(module.vis, Visibility::Public(_)) || module.content.is_some() {
                    continue;
                }
                for path in module_paths(source, module) {
                    if sources.iter().any(|candidate| candidate.path == path) {
                        changed |= public.insert(path);
                    }
                }
            }
        }
        if !changed {
            return public;
        }
    }
}

fn reexported_items(
    sources: &[&Source],
    public_files: &BTreeSet<std::path::PathBuf>,
) -> BTreeMap<std::path::PathBuf, BTreeSet<String>> {
    let mut exposed = BTreeMap::<std::path::PathBuf, BTreeSet<String>>::new();
    for source in sources {
        if !public_files.contains(&source.path) {
            continue;
        }
        for item in &source.syntax.items {
            let Item::Use(item) = item else { continue };
            if !matches!(item.vis, Visibility::Public(_)) {
                continue;
            }
            let mut paths = Vec::new();
            flatten_use(&item.tree, Vec::new(), &mut paths);
            for mut path in paths {
                while matches!(path.first().map(String::as_str), Some("self" | "crate")) {
                    path.remove(0);
                }
                if path.len() < 2 {
                    continue;
                }
                let module = path.remove(0);
                let name = path.remove(0);
                for candidate in module_paths_named(source, &module) {
                    if sources.iter().any(|source| source.path == candidate) {
                        exposed.entry(candidate).or_default().insert(name.clone());
                    }
                }
            }
        }
    }
    exposed
}

fn flatten_use(tree: &UseTree, mut prefix: Vec<String>, paths: &mut Vec<Vec<String>>) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            flatten_use(&path.tree, prefix, paths);
        }
        UseTree::Name(name) => {
            prefix.push(name.ident.to_string());
            paths.push(prefix);
        }
        UseTree::Rename(rename) => {
            prefix.push(rename.ident.to_string());
            paths.push(prefix);
        }
        UseTree::Group(group) => {
            for tree in &group.items {
                flatten_use(tree, prefix.clone(), paths);
            }
        }
        UseTree::Glob(_) => {}
    }
}

fn item_name(item: &Item) -> Option<String> {
    match item {
        Item::Const(item) => Some(item.ident.to_string()),
        Item::Enum(item) => Some(item.ident.to_string()),
        Item::Fn(item) => Some(item.sig.ident.to_string()),
        Item::Static(item) => Some(item.ident.to_string()),
        Item::Struct(item) => Some(item.ident.to_string()),
        Item::Trait(item) => Some(item.ident.to_string()),
        Item::Type(item) => Some(item.ident.to_string()),
        Item::Union(item) => Some(item.ident.to_string()),
        _ => None,
    }
}

fn impl_type_name(item: &ItemImpl) -> Option<String> {
    match item.self_ty.as_ref() {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string()),
        Type::Group(group) => type_name(&group.elem),
        Type::Paren(paren) => type_name(&paren.elem),
        _ => None,
    }
}

fn type_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string()),
        Type::Group(group) => type_name(&group.elem),
        Type::Paren(paren) => type_name(&paren.elem),
        _ => None,
    }
}

fn module_paths(source: &Source, module: &syn::ItemMod) -> [std::path::PathBuf; 2] {
    module_paths_named(source, &module.ident.to_string())
}

fn module_paths_named(source: &Source, name: &str) -> [std::path::PathBuf; 2] {
    let parent = source
        .path
        .parent()
        .unwrap_or_else(|| std::path::Path::new(""));
    let directory = if source.path.file_name().is_some_and(|name| name == "mod.rs")
        || source.path.file_name().is_some_and(|name| name == "lib.rs")
        || source
            .path
            .file_name()
            .is_some_and(|name| name == "main.rs")
    {
        parent.to_owned()
    } else {
        parent.join(source.path.file_stem().unwrap_or_default())
    };
    [
        directory.join(format!("{name}.rs")),
        directory.join(name).join("mod.rs"),
    ]
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use crate::{rule::Rule, Workspace};

    use super::GuiToolkitLeakage;

    fn fixture(package: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "hl-gui-leakage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            format!("[package]\nname = \"{package}\"\nversion = \"0.0.0\"\n"),
        )
        .unwrap();
        root
    }

    fn write(root: &Path, relative: &str, source: &str) {
        let path = root.join("src").join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }

    fn findings(root: &Path) -> Vec<crate::Finding> {
        let workspace = Workspace::load([root.join("src")]).unwrap();
        GuiToolkitLeakage.check(&workspace).unwrap()
    }

    #[test]
    fn detects_nested_native_types_and_import_aliases_in_public_api() {
        let root = fixture("hl-gui");
        write(
            &root,
            "lib.rs",
            r#"
use gtk as native;
use vte4::Terminal as NativeTerminal;

pub struct Public {
    pub callbacks: Vec<Box<dyn Fn(&native::Window) -> Option<NativeTerminal>>>,
}

pub trait Render {
    type Native: glib::ObjectExt;
    fn render<T: Into<gdk::RGBA>>(&self, value: T) -> Result<(), native::glib::Error>;
}
"#,
        );
        let values = findings(&root);
        assert_eq!(values.len(), 3);
        assert!(values.iter().any(|finding| {
            finding.subject == "Public" && finding.message.contains("gtk (native :: Window)")
        }));
        assert!(values.iter().any(|finding| {
            finding.subject == "Render::Native" && finding.message.contains("glib")
        }));
        assert!(values.iter().any(|finding| {
            finding.subject == "Render::render"
                && finding.message.contains("gdk")
                && finding.message.contains("gtk")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_reachable_module_methods_but_ignores_private_surfaces() {
        let root = fixture("hl-gui");
        write(
            &root,
            "lib.rs",
            "pub mod adapter;\nmod private;\npub(crate) mod crate_only;",
        );
        write(
            &root,
            "adapter.rs",
            r#"
pub struct Renderer(gtk::Window);
impl Renderer {
    pub fn native(&self) -> &gtk::Window { &self.0 }
    fn internal(&self, value: gtk::Button) { let _ = value; }
}
pub(crate) fn crate_only() -> gtk::Button { todo!() }
struct Hidden;
impl Hidden { pub fn misleading() -> gtk::Window { todo!() } }
"#,
        );
        write(
            &root,
            "private.rs",
            "pub fn hidden() -> gtk::Window { todo!() }",
        );
        write(
            &root,
            "crate_only.rs",
            "pub fn hidden() -> gtk::Window { todo!() }",
        );
        let values = findings(&root);
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].subject, "Renderer::native");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ignores_other_packages_and_similarly_named_owned_modules() {
        let other = fixture("husklet");
        write(
            &other,
            "lib.rs",
            "pub fn compose(parent: gtk::Window) -> gtk::Window { parent }",
        );
        assert!(findings(&other).is_empty());
        fs::remove_dir_all(other).unwrap();

        let gui = fixture("hl-gui");
        write(
            &gui,
            "lib.rs",
            r#"
pub mod gtk { pub struct Window; }
pub mod glib { pub struct Error; }
pub fn owned(value: crate::gtk::Window) -> crate::glib::Error { todo!() }
"#,
        );
        assert!(findings(&gui).is_empty());
        fs::remove_dir_all(gui).unwrap();
    }

    #[test]
    fn reports_public_aliases_and_qualified_paths_with_signature_context() {
        let root = fixture("hl-gui");
        write(
            &root,
            "lib.rs",
            r#"
pub type Native = Option<gtk::Button>;
pub fn qualified() -> <gtk::Window as Widget>::State { todo!() }
"#,
        );
        let values = findings(&root);
        assert_eq!(values.len(), 2);
        assert!(values.iter().all(|finding| finding
            .review
            .as_ref()
            .unwrap()
            .metadata
            .iter()
            .any(|(key, value)| key == "signature" && !value.is_empty())));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_toolkits_in_declaration_bounds_and_public_reexports() {
        let root = fixture("hl-gui");
        write(
            &root,
            "lib.rs",
            r#"
pub use gtk::Button as NativeButton;
pub struct Generic<T: gtk::glib::ObjectType>(T);
pub trait NativeRender: gdk::prelude::GdkCairoContextExt {}
"#,
        );
        let values = findings(&root);
        assert_eq!(values.len(), 3);
        assert!(values
            .iter()
            .any(|finding| finding.message.contains("re-export")
                && finding.message.contains("gtk (gtk::Button)")));
        assert!(values.iter().any(|finding| {
            finding.subject == "Generic" && finding.message.contains("gtk :: glib :: ObjectType")
        }));
        assert!(values.iter().any(|finding| {
            finding.subject == "NativeRender"
                && finding
                    .message
                    .contains("gdk :: prelude :: GdkCairoContextExt")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn follows_selective_reexports_without_scanning_unexported_items() {
        let root = fixture("hl-gui");
        write(&root, "lib.rs", "mod adapter;\npub use adapter::Exported;");
        write(
            &root,
            "adapter.rs",
            r#"
pub struct Exported;
impl Exported {
    pub fn native(&self) -> gtk::Window { todo!() }
}
pub struct Internal;
impl Internal {
    pub fn native(&self) -> gtk::Button { todo!() }
}
"#,
        );
        let values = findings(&root);
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].subject, "Exported::native");
        fs::remove_dir_all(root).unwrap();
    }
}
