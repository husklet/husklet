use std::collections::HashMap;

use proc_macro2::Span;
use syn::{
    Expr, ExprCall, ExprMacro, ImplItemFn, ItemFn, ItemMod, ItemStatic, ItemUse, Type, UseTree, spanned::Spanned,
    visit::Visit,
};

use crate::{
    Result,
    model::{Finding, Related, Severity},
    rule::Rule,
    source::{Source, Workspace, requires_test},
};

/// Prevents reusable code from obtaining configuration from ambient process state.
pub struct Access;

impl Rule for Access {
    fn id(&self) -> &'static str {
        "environment-variable-access"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for source in workspace.sources() {
            let mut visitor = Accesses::new(source);
            visitor.visit_file(&source.syntax);
            findings.extend(visitor.findings);
        }
        Ok(findings)
    }
}

struct Accesses<'a> {
    source: &'a Source,
    aliases: HashMap<String, Vec<String>>,
    modules: Vec<String>,
    context: Option<(String, Span)>,
    test_depth: usize,
    findings: Vec<Finding>,
}

impl<'a> Accesses<'a> {
    fn new(source: &'a Source) -> Self {
        Self {
            source,
            aliases: HashMap::new(),
            modules: filesystem_modules(source),
            context: None,
            test_depth: 0,
            findings: Vec::new(),
        }
    }

    fn boundary(&self) -> bool {
        self.source.test
            || self.test_depth > 0
            || source_boundary(self.source, &self.modules)
            || self.source.package == "hl-engine"
    }

    fn resolved_path(&self, expression: &syn::ExprPath) -> Vec<String> {
        let mut path = expression
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        if let Some(prefix) = path.first().and_then(|first| self.aliases.get(first)) {
            path.splice(0..1, prefix.clone());
        }
        path
    }

    // The finding owns the subject it reports.
    #[allow(clippy::needless_pass_by_value)]
    fn report_access(&mut self, span: Span, operation: String) {
        if self.boundary() {
            return;
        }
        let context = self
            .context
            .clone()
            .unwrap_or_else(|| ("module scope".to_owned(), span));
        let mut finding = Finding::error(
            "environment-variable-access",
            operation.clone(),
            self.source.location(span),
        );
        finding.message = format!(
            "ambient process input `{operation}` is read outside the application composition root or an explicit platform adapter"
        );
        finding.help = "capture environment and host paths at composition, validate them into typed configuration, and inject the owned value through a domain capability".to_owned();
        finding.related.push(Related {
            label: format!(
                "enclosing `{}` in crate `{}`, module `{}`",
                context.0,
                self.source.package,
                self.modules.join("::")
            ),
            location: self.source.location(context.1),
        });
        self.findings.push(finding);
    }

    fn report_global(&mut self, item: &ItemStatic) {
        if self.boundary() || !ambient_global(item, self) {
            return;
        }
        let name = item.ident.to_string();
        let mut finding = Finding::error(
            "environment-variable-access",
            name.clone(),
            self.source.location(item.span()),
        );
        finding.message = format!("ambient configuration/state global `{name}` hides process-wide input and lifecycle");
        finding.help = "construct this state explicitly at composition and pass an owned configuration or capability to consumers; retain globals only for immutable constants or registries whose process-wide identity is the contract".to_owned();
        finding.related.push(Related {
            label: format!("configuration/state evidence: `{}`", type_text(&item.ty)),
            location: self.source.location(item.ty.span()),
        });
        self.findings.push(finding);
    }
}

impl<'ast> Visit<'ast> for Accesses<'_> {
    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        collect_use(&item.tree, Vec::new(), &mut self.aliases);
        syn::visit::visit_item_use(self, item);
    }

    fn visit_item_mod(&mut self, module: &'ast ItemMod) {
        let aliases = self.aliases.clone();
        self.modules.push(module.ident.to_string());
        self.test_depth += usize::from(requires_test(&module.attrs));
        syn::visit::visit_item_mod(self, module);
        self.test_depth -= usize::from(requires_test(&module.attrs));
        self.modules.pop();
        self.aliases = aliases;
    }

    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        let aliases = self.aliases.clone();
        let test =
            requires_test(&function.attrs) || function.attrs.iter().any(|attribute| attribute.path().is_ident("test"));
        self.test_depth += usize::from(test);
        let previous = self.context.replace((function.sig.ident.to_string(), function.span()));
        syn::visit::visit_item_fn(self, function);
        self.context = previous;
        self.test_depth -= usize::from(test);
        self.aliases = aliases;
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        let aliases = self.aliases.clone();
        let test =
            requires_test(&function.attrs) || function.attrs.iter().any(|attribute| attribute.path().is_ident("test"));
        self.test_depth += usize::from(test);
        let previous = self.context.replace((function.sig.ident.to_string(), function.span()));
        syn::visit::visit_impl_item_fn(self, function);
        self.context = previous;
        self.test_depth -= usize::from(test);
        self.aliases = aliases;
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Expr::Path(function) = call.func.as_ref() {
            let path = self.resolved_path(function);
            if ambient_call(&path) {
                self.report_access(call.span(), path.join("::"));
            }
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_macro(&mut self, expression: &'ast ExprMacro) {
        syn::visit::visit_expr_macro(self, expression);
    }

    fn visit_item_static(&mut self, item: &'ast ItemStatic) {
        self.report_global(item);
        syn::visit::visit_item_static(self, item);
    }
}

fn ambient_call(path: &[String]) -> bool {
    matches!(
        path,
        [std, env, operation]
            if std == "std"
                && env == "env"
                && matches!(
                    operation.as_str(),
                    "var" | "var_os" | "vars" | "vars_os" | "set_var" | "remove_var"
                        | "current_dir" | "current_exe" | "temp_dir"
                )
    ) || matches!(
        path,
        [dirs, operation]
            if dirs == "dirs"
                && matches!(
                    operation.as_str(),
                    "home_dir" | "audio_dir" | "cache_dir" | "config_dir" | "config_local_dir"
                        | "data_dir" | "data_local_dir" | "desktop_dir" | "document_dir"
                        | "download_dir" | "executable_dir" | "font_dir" | "picture_dir"
                        | "preference_dir" | "public_dir" | "runtime_dir" | "state_dir"
                        | "template_dir" | "video_dir"
                )
    )
}

fn ambient_global(item: &ItemStatic, accesses: &Accesses<'_>) -> bool {
    let name = item.ident.to_string().to_ascii_lowercase();
    let ty = type_text(&item.ty).to_ascii_lowercase();
    let semantic_name = ["config", "configuration", "settings", "state"]
        .iter()
        .any(|word| name.contains(word) || ty.contains(word));
    if !semantic_name {
        return false;
    }
    let lazy = ["oncelock", "lazylock", "oncecell", "lazy"]
        .iter()
        .any(|kind| ty.contains(kind));
    lazy || has_ambient_input(&item.expr, accesses)
}

fn has_ambient_input(expression: &Expr, accesses: &Accesses<'_>) -> bool {
    struct Calls<'a, 'b> {
        accesses: &'a Accesses<'b>,
        found: bool,
    }
    impl<'ast> Visit<'ast> for Calls<'_, '_> {
        fn visit_expr_call(&mut self, call: &'ast ExprCall) {
            if let Expr::Path(path) = call.func.as_ref() {
                self.found |= ambient_call(&self.accesses.resolved_path(path));
            }
            syn::visit::visit_expr_call(self, call);
        }
        fn visit_expr_macro(&mut self, expression: &'ast ExprMacro) {
            syn::visit::visit_expr_macro(self, expression);
        }
    }
    let mut calls = Calls { accesses, found: false };
    calls.visit_expr(expression);
    calls.found
}

fn type_text(ty: &Type) -> String {
    struct TypeNames(Vec<String>);
    impl<'ast> Visit<'ast> for TypeNames {
        fn visit_path_segment(&mut self, segment: &'ast syn::PathSegment) {
            self.0.push(segment.ident.to_string());
            syn::visit::visit_path_segment(self, segment);
        }
    }
    let mut names = TypeNames(Vec::new());
    names.visit_type(ty);
    names.0.join("::")
}

fn collect_use(tree: &UseTree, prefix: Vec<String>, aliases: &mut HashMap<String, Vec<String>>) {
    match tree {
        UseTree::Path(path) => {
            let mut prefix = prefix;
            prefix.push(path.ident.to_string());
            collect_use(&path.tree, prefix, aliases);
        }
        UseTree::Name(name) => {
            let mut target = prefix;
            if name.ident != "self" {
                target.push(name.ident.to_string());
            }
            aliases.insert(name.ident.to_string(), target);
        }
        UseTree::Rename(rename) => {
            let mut target = prefix;
            if rename.ident != "self" {
                target.push(rename.ident.to_string());
            }
            aliases.insert(rename.rename.to_string(), target);
        }
        UseTree::Group(group) => {
            for tree in &group.items {
                collect_use(tree, prefix.clone(), aliases);
            }
        }
        UseTree::Glob(_) => {}
    }
}

fn filesystem_modules(source: &Source) -> Vec<String> {
    let Some(root) = source
        .path
        .ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == "src"))
    else {
        return Vec::new();
    };
    source
        .path
        .strip_prefix(root)
        .ok()
        .into_iter()
        .flat_map(|path| path.components())
        .filter_map(|component| component.as_os_str().to_str())
        .map(|part| part.trim_end_matches(".rs").to_owned())
        .filter(|part| !matches!(part.as_str(), "lib" | "main" | "mod"))
        .collect()
}

fn source_boundary(source: &Source, modules: &[String]) -> bool {
    let relative = package_relative(source);
    let parts = relative
        .as_ref()
        .into_iter()
        .flat_map(|path| path.components())
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    // `src/main.rs` is the composition root this rule's own message exempts.
    let executable = matches!(parts.as_slice(), ["build.rs"])
        || matches!(parts.as_slice(), ["examples", ..])
        || matches!(parts.as_slice(), ["src", "bin", ..])
        || matches!(parts.as_slice(), ["src", "main.rs"]);
    let explicit_module = modules.first().is_some_and(|module| {
        matches!(
            module.as_str(),
            "adapter" | "adapters" | "platform" | "host" | "unix" | "windows" | "macos" | "linux"
        )
    }) || matches!(
        modules,
        [surface, platform, ..]
            if surface == "surface"
                && matches!(platform.as_str(), "macos" | "windows" | "linux")
    );
    let shim = source.package.contains("-shim-")
        && source
            .path
            .components()
            .any(|component| component.as_os_str().to_str().is_some_and(|part| part == "shim"));
    let concrete_wgpu = source.package == "hl-gpu-wgpu"
        && source
            .path
            .components()
            .any(|component| component.as_os_str().to_str().is_some_and(|part| part == "gpu"))
        && source
            .path
            .components()
            .any(|component| component.as_os_str().to_str().is_some_and(|part| part == "hl-gpu-wgpu"));
    executable || composition_root(source) || explicit_module || shim || concrete_wgpu
}

/// Reports whether the source belongs to a composition root, which owns environment capture.
///
/// `src/apps/*` is the application layer and `hl-engine` is the engine composition root; both
/// are named as owners of configuration and environment capture. Nothing depends on them, so
/// they are not the reusable code this rule protects. Locating the capture in `main.rs` is a
/// file-layout choice, not the property that makes it legitimate.
fn composition_root(source: &Source) -> bool {
    source
        .path
        .components()
        .zip(source.path.components().skip(1))
        .any(|(layer, package)| layer.as_os_str() == "src" && package.as_os_str() == "apps")
}

fn package_relative(source: &Source) -> Option<std::path::PathBuf> {
    source
        .path
        .ancestors()
        .find(|directory| directory.join("Cargo.toml").is_file())
        .and_then(|root| source.path.strip_prefix(root).ok())
        .map(std::path::Path::to_owned)
}

#[cfg(test)]
#[path = "test.rs"]
mod tests;
