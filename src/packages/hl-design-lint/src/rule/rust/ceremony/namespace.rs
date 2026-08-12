use std::path::PathBuf;

use syn::{Attribute, Item, ItemMod, UseTree, Visibility, spanned::Spanned, visit::Visit};

use crate::{
    model::{Finding, Related, Review},
    source::{Source, Workspace},
};

use super::ID;

pub(super) fn findings(workspace: &Workspace) -> Vec<Finding> {
    workspace
        .production()
        .filter_map(|source| candidate(source, workspace))
        .collect()
}

fn candidate(source: &Source, workspace: &Workspace) -> Option<Finding> {
    if source.path.file_name()?.to_str()? != "mod.rs"
        || !source.syntax.attrs.is_empty()
        || source.syntax.items.len() < 2
    {
        return None;
    }

    let children = source
        .syntax
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Mod(module) if module.content.is_none() => Some(module),
            _ => None,
        })
        .collect::<Vec<_>>();
    if children.len() != 1 {
        return None;
    }
    let child = children[0];
    if !child.attrs.is_empty() {
        return None;
    }
    let child_name = child.ident.to_string();
    let uses = source
        .syntax
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Use(item) => Some(item),
            Item::Mod(_) => None,
            _ => None,
        })
        .collect::<Vec<_>>();
    if uses.is_empty()
        || uses.iter().any(|item| {
            !item.attrs.is_empty() || !crate_visible(&item.vis) || !transparent_use(&item.tree, &child_name)
        })
    {
        return None;
    }

    let module_name = source.path.parent()?.file_name()?.to_str()?.to_owned();
    let declaration = parent_declaration(source, workspace, &module_name)?;
    if !matches!(declaration.item.vis, Visibility::Inherited)
        || !declaration.item.attrs.is_empty()
        || workspace
            .production()
            .any(|other| other.path != source.path && has_qualified_use(other, &module_name))
    {
        return None;
    }

    let child_path = child_path(source, child)?;
    let child_source = workspace.production().find(|candidate| candidate.path == child_path)?;
    if child_source.syntax.attrs.iter().any(boundary_attribute) || child_source.syntax.items.iter().any(boundary_item) {
        return None;
    }

    let mut finding = Finding::warning(ID, module_name.clone(), source.location(child.span()));
    finding.message =
        format!("private module `{module_name}` contains only child `{child_name}` and transparent re-exports");
    finding.help = "flatten the child into the parent module unless this namespace owns a documented public, platform, generation, FFI, cfg, or privacy contract".into();
    finding.related = vec![
        Related {
            label: "private parent declaration".into(),
            location: declaration.source.location(declaration.item.span()),
        },
        Related {
            label: "sole child implementation".into(),
            location: child_source.location(child_source.syntax.span()),
        },
    ];
    let mut review = Review::error();
    review.metadata = vec![
        ("Category".into(), "single-child transparent namespace".into()),
        ("Child".into(), child_name),
        (
            "Re-exports".into(),
            uses.iter()
                .map(|item| source.excerpt(item.span()))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        (
            "Qualified external uses".into(),
            "none found in the owning workspace".into(),
        ),
    ];
    review.questions = vec![
        "Does this namespace enforce any visibility or compatibility contract not visible in the source?".into(),
        "Can the child file replace mod.rs without changing supported paths?".into(),
    ];
    finding.review = Some(review);
    Some(finding)
}

struct Declaration<'a> {
    source: &'a Source,
    item: &'a ItemMod,
}

fn parent_declaration<'a>(source: &Source, workspace: &'a Workspace, module_name: &str) -> Option<Declaration<'a>> {
    let directory = source.path.parent()?.parent()?;
    workspace.production().find_map(|candidate| {
        if candidate.path.parent()? != directory {
            return None;
        }
        candidate.syntax.items.iter().find_map(|item| match item {
            Item::Mod(module) if module.content.is_none() && module.ident == module_name => Some(Declaration {
                source: candidate,
                item: module,
            }),
            _ => None,
        })
    })
}

fn transparent_use(tree: &UseTree, child: &str) -> bool {
    match tree {
        UseTree::Path(path) => path.ident == child,
        _ => false,
    }
}

fn crate_visible(visibility: &Visibility) -> bool {
    let Visibility::Restricted(restricted) = visibility else {
        return false;
    };
    restricted.path.is_ident("crate")
}

fn child_path(source: &Source, child: &ItemMod) -> Option<PathBuf> {
    let directory = source.path.parent()?;
    let file = directory.join(format!("{}.rs", child.ident));
    if file.is_file() {
        return Some(file);
    }
    let module = directory.join(child.ident.to_string()).join("mod.rs");
    module.is_file().then_some(module)
}

fn boundary_attribute(attribute: &Attribute) -> bool {
    let name = attribute.path().segments.last().map(|part| part.ident.to_string());
    matches!(
        name.as_deref(),
        Some("cfg" | "cfg_attr" | "path" | "link" | "repr" | "doc")
    )
}

fn boundary_item(item: &Item) -> bool {
    match item {
        Item::ForeignMod(_) | Item::Macro(_) => true,
        _ => item_attrs(item).iter().any(boundary_attribute),
    }
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        Item::Verbatim(_) | _ => &[],
    }
}

fn has_qualified_use(source: &Source, module: &str) -> bool {
    struct Paths<'a> {
        module: &'a str,
        found: bool,
    }
    impl<'ast> Visit<'ast> for Paths<'_> {
        fn visit_path(&mut self, path: &'ast syn::Path) {
            if path.segments.len() > 1
                && path
                    .segments
                    .first()
                    .is_some_and(|segment| segment.ident == self.module)
            {
                self.found = true;
            }
            syn::visit::visit_path(self, path);
        }
    }
    let mut paths = Paths { module, found: false };
    paths.visit_file(&source.syntax);
    paths.found
}
