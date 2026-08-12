use std::collections::{BTreeMap, BTreeSet};

use syn::{Item, Path, UseTree};

use crate::source::Source;

use super::TOOLKITS;

#[derive(Default)]
pub(super) struct Aliases {
    crates: BTreeMap<String, String>,
    types: BTreeMap<String, String>,
}

impl Aliases {
    pub(super) fn from_source(source: &Source) -> Self {
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
            aliases.collect_item(item);
        }
        aliases
    }

    fn collect_item(&mut self, item: &Item) {
        match item {
            Item::Use(item) => collect_use(&item.tree, None, self),
            Item::ExternCrate(item) => self.collect_extern(item),
            _ => {}
        }
    }

    fn collect_extern(&mut self, item: &syn::ItemExternCrate) {
        let original = item.ident.to_string();
        if !TOOLKITS.contains(&original.as_str()) {
            return;
        }
        let local = item
            .rename
            .as_ref()
            .map_or_else(|| original.clone(), |(_, name)| name.to_string());
        self.crates.insert(local, original);
    }

    pub(super) fn toolkit(&self, path: &Path) -> Option<String> {
        let first = path.segments.first()?.ident.to_string();
        if let Some(toolkit) = self.crates.get(&first) {
            return Some(toolkit.clone());
        }
        self.types.get(&first).cloned()
    }

    pub(super) fn crate_toolkit(&self, name: &str) -> Option<&str> {
        self.crates.get(name).map(String::as_str)
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
