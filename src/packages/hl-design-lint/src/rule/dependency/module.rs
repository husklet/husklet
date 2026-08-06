use std::collections::HashSet;

use proc_macro2::Span;
use syn::{Path as RustPath, UseTree, spanned::Spanned, visit::Visit};

use crate::{
    model::{Finding, Review},
    source::{Source, Workspace},
};

use super::discovery::owning_manifest;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Role {
    Model,
    Api,
    Library,
    Service,
    Ports,
    Adapters,
}

impl Role {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "model" => Some(Self::Model),
            "api" => Some(Self::Api),
            "lib" => Some(Self::Library),
            "service" => Some(Self::Service),
            "ports" => Some(Self::Ports),
            "adapters" => Some(Self::Adapters),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Api => "api",
            Self::Library => "lib",
            Self::Service => "service",
            Self::Ports => "ports",
            Self::Adapters => "adapters",
        }
    }

    fn permits(self, target: Self) -> bool {
        match self {
            Self::Model => target == Self::Model,
            Self::Library => matches!(target, Self::Library),
            Self::Ports => matches!(target, Self::Model | Self::Library | Self::Ports),
            Self::Service => matches!(target, Self::Model | Self::Library | Self::Ports | Self::Service),
            Self::Api => matches!(target, Self::Model | Self::Service | Self::Api),
            Self::Adapters => matches!(target, Self::Model | Self::Library | Self::Ports | Self::Adapters),
        }
    }
}

fn source_role(source: &Source) -> Option<Role> {
    let manifest = owning_manifest(&source.path)?;
    let relative = source.path.strip_prefix(manifest.parent()?).ok()?;
    let mut components = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str());
    if components.next()? != "src" {
        return None;
    }
    let first = components.next()?;
    Role::parse(first.strip_suffix(".rs").unwrap_or(first))
}

pub(super) fn findings(workspace: &Workspace, rule: &'static str) -> Vec<Finding> {
    let mut findings = Vec::new();
    for source in workspace.production() {
        let Some(role) = source_role(source) else {
            continue;
        };
        let mut visitor = RoleVisitor {
            source,
            role,
            rule,
            findings: &mut findings,
            seen: HashSet::new(),
        };
        visitor.visit_file(&source.syntax);
    }
    findings
}

struct RoleVisitor<'a> {
    source: &'a Source,
    role: Role,
    rule: &'static str,
    findings: &'a mut Vec<Finding>,
    seen: HashSet<(usize, usize, Role)>,
}

impl RoleVisitor<'_> {
    fn record(&mut self, target: Role, span: Span) {
        if self.role.permits(target) {
            return;
        }
        let start = span.start();
        if !self.seen.insert((start.line, start.column, target)) {
            return;
        }
        let mut finding = Finding::error(
            self.rule,
            format!("{} -> {}", self.role.label(), target.label()),
            self.source.location(span),
        );
        finding.message = format!(
            "`{}` code imports the inward-incompatible `{}` role through an explicit crate-root path",
            self.role.label(),
            target.label(),
        );
        finding.help = role_help(self.role, target).into();
        let mut review = Review::error();
        review.metadata.extend([
            ("Source role".into(), self.role.label().into()),
            ("Target role".into(), target.label().into()),
            ("Proof".into(), "explicit `crate::` path".into()),
        ]);
        review.questions.push(
            "Does this behavior belong in the target role, or should a lower model/port own the contract?".into(),
        );
        finding.review = Some(review);
        self.findings.push(finding);
    }
}

impl<'ast> Visit<'ast> for RoleVisitor<'_> {
    fn visit_path(&mut self, path: &'ast RustPath) {
        let mut segments = path.segments.iter();
        if segments.next().is_some_and(|segment| segment.ident == "crate")
            && let Some(target) = segments
                .next()
                .and_then(|segment| Role::parse(&segment.ident.to_string()))
            {
                self.record(target, path.span());
            }
        syn::visit::visit_path(self, path);
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        if let UseTree::Path(root) = &item.tree
            && root.ident == "crate" {
                collect_use_roles(&root.tree, &mut |target| self.record(target, item.span()));
            }
        syn::visit::visit_item_use(self, item);
    }
}

fn collect_use_roles(tree: &UseTree, record: &mut impl FnMut(Role)) {
    match tree {
        UseTree::Path(path) => {
            if let Some(role) = Role::parse(&path.ident.to_string()) {
                record(role);
            }
        }
        UseTree::Name(name) => {
            if let Some(role) = Role::parse(&name.ident.to_string()) {
                record(role);
            }
        }
        UseTree::Group(group) => {
            for item in &group.items {
                collect_use_roles(item, record);
            }
        }
        UseTree::Rename(rename) => {
            if let Some(role) = Role::parse(&rename.ident.to_string()) {
                record(role);
            }
        }
        UseTree::Glob(_) => {}
    }
}

fn role_help(source: Role, target: Role) -> &'static str {
    match (source, target) {
        (Role::Model, _) => "keep models independent of transport, orchestration, ports, and concrete adapters",
        (Role::Library, _) => "keep transferable domain-kind machinery independent of domain policy and I/O roles",
        (Role::Service, Role::Adapters) => {
            "depend on a narrow domain-owned port and select the concrete adapter at composition"
        }
        (Role::Api, _) => "delegate through models or services instead of reaching across API boundaries",
        (Role::Ports, _) => "express the capability with model/library values, not higher-level behavior",
        (Role::Adapters, _) => "implement a domain-owned port without depending on API or service policy",
        _ => "move the dependency toward the lower role that owns the contract",
    }
}
