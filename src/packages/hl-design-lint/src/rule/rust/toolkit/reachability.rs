use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use syn::{Item, ItemImpl, UseTree, Visibility};

use crate::{rule::support::syntax::type_name, source::Source};

pub(super) fn public_files(sources: &[&Source]) -> BTreeSet<PathBuf> {
    let mut public = sources
        .iter()
        .filter(|source| matches!(source.path.file_name().and_then(|name| name.to_str()), Some("lib.rs")))
        .map(|source| source.path.clone())
        .collect::<BTreeSet<_>>();
    loop {
        let additions = reachable_module_files(&public, sources);
        let changed = additions
            .into_iter()
            .fold(false, |changed, path| public.insert(path) || changed);
        if !changed {
            return public;
        }
    }
}

fn reachable_module_files(public: &BTreeSet<PathBuf>, sources: &[&Source]) -> Vec<PathBuf> {
    sources
        .iter()
        .filter(|source| public.contains(&source.path))
        .flat_map(|source| public_module_files(source, sources))
        .collect()
}

fn public_module_files(source: &Source, sources: &[&Source]) -> Vec<PathBuf> {
    source
        .syntax
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Mod(module) if matches!(module.vis, Visibility::Public(_)) && module.content.is_none() => {
                Some(module)
            }
            _ => None,
        })
        .flat_map(|module| module_paths(source, module))
        .filter(|path| sources.iter().any(|candidate| candidate.path == *path))
        .collect()
}

pub(super) fn reexported_items(
    sources: &[&Source],
    public_files: &BTreeSet<PathBuf>,
) -> BTreeMap<PathBuf, BTreeSet<String>> {
    let mut exposed = BTreeMap::<PathBuf, BTreeSet<String>>::new();
    for (path, name) in public_reexports(sources, public_files) {
        exposed.entry(path).or_default().insert(name);
    }
    exposed
}

fn public_reexports(sources: &[&Source], public_files: &BTreeSet<PathBuf>) -> Vec<(PathBuf, String)> {
    sources
        .iter()
        .filter(|source| public_files.contains(&source.path))
        .flat_map(|source| source_reexports(source, sources))
        .collect()
}

fn source_reexports(source: &Source, sources: &[&Source]) -> Vec<(PathBuf, String)> {
    source
        .syntax
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Use(item) if matches!(item.vis, Visibility::Public(_)) => Some(item),
            _ => None,
        })
        .flat_map(|item| {
            let mut paths = Vec::new();
            flatten_use(&item.tree, Vec::new(), &mut paths);
            paths
        })
        .filter_map(normalized_reexport)
        .flat_map(|(module, name)| {
            module_paths_named(source, &module)
                .into_iter()
                .filter(|candidate| sources.iter().any(|source| source.path == *candidate))
                .map(move |candidate| (candidate, name.clone()))
        })
        .collect()
}

fn normalized_reexport(mut path: Vec<String>) -> Option<(String, String)> {
    while matches!(path.first().map(String::as_str), Some("self" | "crate")) {
        path.remove(0);
    }
    (path.len() >= 2).then(|| (path.remove(0), path.remove(0)))
}

pub(super) fn item_name(item: &Item) -> Option<String> {
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

pub(super) fn impl_type_name(item: &ItemImpl) -> Option<String> {
    type_name(&item.self_ty)
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

fn module_paths(source: &Source, module: &syn::ItemMod) -> [PathBuf; 2] {
    module_paths_named(source, &module.ident.to_string())
}

fn module_paths_named(source: &Source, name: &str) -> [PathBuf; 2] {
    let parent = source.path.parent().unwrap_or_else(|| Path::new(""));
    let directory = if source.path.file_name().is_some_and(|name| name == "mod.rs")
        || source.path.file_name().is_some_and(|name| name == "lib.rs")
        || source.path.file_name().is_some_and(|name| name == "main.rs")
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
