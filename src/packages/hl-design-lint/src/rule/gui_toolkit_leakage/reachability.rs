use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use syn::{Item, ItemImpl, Type, UseTree, Visibility};

use crate::source::Source;

pub(super) fn public_files(sources: &[&Source]) -> BTreeSet<PathBuf> {
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

pub(super) fn reexported_items(
    sources: &[&Source],
    public_files: &BTreeSet<PathBuf>,
) -> BTreeMap<PathBuf, BTreeSet<String>> {
    let mut exposed = BTreeMap::<PathBuf, BTreeSet<String>>::new();
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

fn module_paths(source: &Source, module: &syn::ItemMod) -> [PathBuf; 2] {
    module_paths_named(source, &module.ident.to_string())
}

fn module_paths_named(source: &Source, name: &str) -> [PathBuf; 2] {
    let parent = source.path.parent().unwrap_or_else(|| Path::new(""));
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
