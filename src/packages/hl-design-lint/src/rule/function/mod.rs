use std::collections::{BTreeSet, HashMap, HashSet};

use syn::{Expr, ExprCall, ExprMacro, FnArg, ItemFn, ItemMod, PathArguments, Type, spanned::Spanned, visit::Visit};

use crate::{
    Result,
    model::{Finding, Related, Review, ReviewState, Severity},
    rule::{Rule, references::References},
    source::{Workspace, platform_gated, requires_test},
};

/// Requires a free function whose sole argument is a value this tree declares to become a method on
/// that type, since the argument is already the receiver.
pub struct FreeFunction;

impl Rule for FreeFunction {
    fn id(&self) -> &'static str {
        "unclassified-free-function"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let owned = owned_types(workspace);
        let crates = owned_crates(workspace);
        let mut candidates = Vec::new();
        let mut definitions = HashMap::<String, usize>::new();
        let mut references = HashMap::<String, Vec<crate::rule::references::Reference>>::new();
        let mut gated = HashSet::new();
        for source in workspace.production() {
            let mut functions = Functions {
                path: &source.path,
                owned: &owned,
                crates: &crates,
                imports: imports(source),
                package: source.package.replace('-', "_"),
                test_scope: false,
                nesting: Vec::new(),
                values: Vec::new(),
            };
            functions.visit_file(&source.syntax);
            for value in functions.values {
                // Only one `#[cfg(target_os = ...)]` sibling exists in any build, so the set is one
                // logical function: counting each would forge an ambiguous name and drop its usages.
                if value.platform && !gated.insert((source.path.clone(), value.name.clone())) {
                    continue;
                }
                *definitions.entry(value.name.clone()).or_default() += 1;
                candidates.push((source, value));
            }
            let mut uses = References::new(source);
            uses.visit_file(&source.syntax);
            for reference in uses.values {
                references.entry(reference.name.clone()).or_default().push(reference);
            }
        }
        Ok(candidates
            .into_iter()
            .map(|(source, candidate)| {
                let ambiguous = definitions.get(&candidate.name).copied().unwrap_or(0) != 1;
                let owner = candidate.module.clone();
                let usages = references
                    .get(&candidate.name)
                    .into_iter()
                    .flatten()
                    .filter(|usage| usage.module.as_ref().is_none_or(|module| *module == owner))
                    .filter(|usage| !ambiguous || usage.location.path == source.path);
                candidate.finding(self.id(), source, usages, ambiguous)
            })
            .collect())
    }
}

struct Candidate {
    name: String,
    arguments: usize,
    span: proc_macro2::Span,
    module: String,
    dependencies: Vec<String>,
    classification: Option<Classification>,
    platform: bool,
}

impl Candidate {
    fn finding<'a>(
        self,
        rule: &'static str,
        source: &crate::source::Source,
        usages: impl Iterator<Item = &'a crate::rule::references::Reference>,
        ambiguous: bool,
    ) -> Finding {
        let mut finding = Finding::error(rule, &self.name, source.location(self.span));
        finding.message = format!(
            "free function `{}` takes one declared type, which is the receiver a method would take",
            self.name
        );
        finding.help = "make it a method on that type or add a temporary #[hl_design::classify(...)] classification".into();
        finding.related = usages
            .map(|usage| Related {
                label: usage.context.as_ref().map_or_else(
                    || "usage at module scope".into(),
                    |context| format!("usage in `{}`\n{}", context.name, context.source),
                ),
                location: usage.location.clone(),
            })
            .collect();
        let (state, classification) = self.classification.map_or_else(
            || (ReviewState::Error, "unclassified".to_owned()),
            |value| {
                let value = value.resolve(&source.package);
                let text = format!("{}({})", value.scope, value.kind);
                (ReviewState::Check(text.clone()), text)
            },
        );
        let review = Review {
            state,
            metadata: vec![
                ("Arguments".into(), self.arguments.to_string()),
                ("Classification".into(), classification),
                (
                    "Usage resolution".into(),
                    if ambiguous {
                        "ambiguous name; same-file references only"
                    } else {
                        "unique name in scanned tree"
                    }
                    .into(),
                ),
            ],
            dependencies: self.dependencies,
            questions: vec![
                "Does this express the argument's own behavior, or a transformation over it?".into(),
                "Do related functions share this value and its invariants?".into(),
                "Would the type's crate be the wrong owner for this behavior?".into(),
                "Is this a complete low-level algorithm that should remain free?".into(),
            ],
        };
        finding.review = Some(review);
        finding
    }
}

struct Functions<'a> {
    path: &'a std::path::Path,
    owned: &'a HashSet<String>,
    crates: &'a HashSet<String>,
    imports: HashMap<String, String>,
    package: String,
    test_scope: bool,
    nesting: Vec<String>,
    values: Vec<Candidate>,
}

impl<'ast> Visit<'ast> for Functions<'_> {
    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        if !self.test_scope
            && !requires_test(&function.attrs)
            && candidate(function)
            && receives_owned(function, &self.scope())
        {
            let mut dependencies = Dependencies::default();
            dependencies.visit_item_fn(function);
            let span = function
                .attrs
                .first()
                .and_then(|attribute| attribute.span().join(function.span()))
                .unwrap_or_else(|| function.span());
            self.values.push(Candidate {
                name: function.sig.ident.to_string(),
                arguments: function.sig.inputs.len(),
                span,
                module: crate::rule::references::module(self.path, &self.nesting),
                dependencies: dependencies.names.into_iter().collect(),
                classification: classification(function),
                platform: platform_gated(&function.attrs),
            });
        }
        syn::visit::visit_item_fn(self, function);
    }

    fn visit_item_mod(&mut self, module: &'ast ItemMod) {
        let previous = self.test_scope;
        self.test_scope |= requires_test(&module.attrs);
        self.nesting.push(module.ident.to_string());
        syn::visit::visit_item_mod(self, module);
        self.nesting.pop();
        self.test_scope = previous;
    }
}

/// Names every type the scanned tree declares, so a free function can be asked whether a receiver
/// for it exists at all.
fn owned_types(workspace: &Workspace) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut parsed = HashSet::new();
    for source in workspace.production() {
        let mut declarations = Declarations {
            names: &mut names,
            parsed: &mut parsed,
        };
        declarations.visit_file(&source.syntax);
    }
    // A command-line argument type is the boundary value a composition root is handed, not an entity
    // with behavior, so its entry point is correctly free.
    names.retain(|name| !parsed.contains(name));
    names
}

/// Every crate name in the scanned tree, in the underscored spelling a path segment uses.
fn owned_crates(workspace: &Workspace) -> HashSet<String> {
    workspace
        .production()
        .map(|source| source.package.replace('-', "_"))
        .collect()
}

/// What a source file's `use` items bind each name to, keyed by the bound name and valued by the
/// crate the path starts at. Without this a bare `Path` cannot be told from `std::path::Path`.
fn imports(source: &crate::source::Source) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for item in &source.syntax.items {
        if let syn::Item::Use(item) = item {
            collect_use(&item.tree, None, &mut values);
        }
    }
    values
}

fn collect_use(tree: &syn::UseTree, root: Option<&str>, values: &mut HashMap<String, String>) {
    match tree {
        syn::UseTree::Path(path) => {
            let segment = path.ident.to_string();
            let root = root.unwrap_or(&segment).to_owned();
            collect_use(&path.tree, Some(&root), values);
        }
        syn::UseTree::Name(name) => {
            if let Some(root) = root {
                values.insert(name.ident.to_string(), root.to_owned());
            }
        }
        syn::UseTree::Rename(rename) => {
            if let Some(root) = root {
                values.insert(rename.rename.to_string(), root.to_owned());
            }
        }
        syn::UseTree::Group(group) => {
            for tree in &group.items {
                collect_use(tree, root, values);
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}

/// Resolves the argument types of one function against the scanned tree.
struct Scope<'a> {
    owned: &'a HashSet<String>,
    crates: &'a HashSet<String>,
    imports: &'a HashMap<String, String>,
    package: &'a str,
}

impl Scope<'_> {
    /// Whether a written type path names a type this tree declares. A leading segment that resolves
    /// outside the workspace disqualifies it, so `std::path::Path` is not the tree's own `Path`.
    fn declares(&self, path: &syn::Path) -> bool {
        let Some(last) = path.segments.last() else {
            return false;
        };
        if !self.owned.contains(&last.ident.to_string()) {
            return false;
        }
        let first = path.segments[0].ident.to_string();
        let root = if path.segments.len() > 1 {
            match first.as_str() {
                "crate" | "self" | "super" => self.package.to_owned(),
                _ => first,
            }
        } else {
            self.imports.get(&first).cloned().unwrap_or_else(|| self.package.to_owned())
        };
        self.crates.contains(&root)
    }
}

impl Functions<'_> {
    fn scope(&self) -> Scope<'_> {
        Scope {
            owned: self.owned,
            crates: self.crates,
            imports: &self.imports,
            package: &self.package,
        }
    }
}

struct Declarations<'a> {
    names: &'a mut HashSet<String>,
    parsed: &'a mut HashSet<String>,
}

impl<'ast> Visit<'ast> for Declarations<'_> {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        match item {
            syn::Item::Struct(value) => {
                self.names.insert(value.ident.to_string());
                if command_line(&value.attrs) {
                    self.parsed.insert(value.ident.to_string());
                }
            }
            syn::Item::Enum(value) => {
                self.names.insert(value.ident.to_string());
                if command_line(&value.attrs) {
                    self.parsed.insert(value.ident.to_string());
                }
            }
            syn::Item::Union(value) => drop(self.names.insert(value.ident.to_string())),
            syn::Item::Type(value) => drop(self.names.insert(value.ident.to_string())),
            syn::Item::Trait(value) => drop(self.names.insert(value.ident.to_string())),
            _ => {}
        }
        syn::visit::visit_item(self, item);
    }
}

/// Whether a type is parsed from the command line, which clap signals through its derives.
fn command_line(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("derive")
            && attribute
                .parse_args_with(syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated)
                .is_ok_and(|derives| {
                    derives.iter().any(|derive| {
                        derive
                            .segments
                            .last()
                            .is_some_and(|segment| matches!(segment.ident.to_string().as_str(), "Parser" | "Args"))
                    })
                })
    })
}

/// Reports whether the function's sole argument *is* a value this tree declares, so the argument is
/// already the receiver the method form would take. A wrapped or collected argument (`Vec<T>`,
/// `Option<T>`, `Result<T, E>`, `&[T]`) is a transformation over many values and has no receiver, and
/// a second argument means the function relates two things rather than expressing one thing's
/// behavior. Both shapes are correctly free in Rust.
fn receives_owned(function: &ItemFn, scope: &Scope<'_>) -> bool {
    let [argument] = function.sig.inputs.iter().collect::<Vec<_>>()[..] else {
        return false;
    };
    let FnArg::Typed(argument) = argument else {
        return true;
    };
    let mut ty = argument.ty.as_ref();
    while let Type::Reference(reference) = ty {
        ty = reference.elem.as_ref();
    }
    matches!(ty, Type::Path(path) if path.path.segments.last().is_some_and(|segment|
        matches!(segment.arguments, PathArguments::None)) && scope.declares(&path.path))
}

fn candidate(function: &ItemFn) -> bool {
    function.sig.abi.is_none()
        && matches!(function.sig.inputs.len(), 1 | 2)
        && !framework_adapter(function)
        && !function.attrs.iter().any(|attribute| {
            let path = attribute.path();
            path.is_ident("proc_macro") || path.is_ident("proc_macro_attribute") || path.is_ident("proc_macro_derive")
        })
}

fn framework_adapter(function: &ItemFn) -> bool {
    if !function.attrs.iter().any(|attribute| {
        attribute
            .path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "adapter")
    }) {
        return false;
    }
    extractor_argument(function) || value_parser_shape(function)
}

fn extractor_argument(function: &ItemFn) -> bool {
    function.sig.inputs.iter().any(|argument| {
        let FnArg::Typed(argument) = argument else {
            return false;
        };
        let Type::Path(ty) = argument.ty.as_ref() else {
            return false;
        };
        ty.path.segments.last().is_some_and(|segment| {
            let name = segment.ident.to_string();
            let generic = matches!(segment.arguments, PathArguments::AngleBracketed(_));
            (generic
                && matches!(
                    name.as_str(),
                    "State" | "Path" | "Query" | "Json" | "Form" | "Extension" | "ConnectInfo"
                ))
                || matches!(
                    name.as_str(),
                    "OriginalUri" | "RawQuery" | "WebSocketUpgrade" | "Multipart"
                )
        })
    })
}

/// clap owns the value-parser signature: one `&str` in, a `Result` out.
fn value_parser_shape(function: &ItemFn) -> bool {
    let [FnArg::Typed(argument)] = function.sig.inputs.iter().collect::<Vec<_>>()[..] else {
        return false;
    };
    let Type::Reference(reference) = argument.ty.as_ref() else {
        return false;
    };
    if !matches!(reference.elem.as_ref(), Type::Path(inner) if inner.path.is_ident("str")) {
        return false;
    }
    let syn::ReturnType::Type(_, ty) = &function.sig.output else {
        return false;
    };
    matches!(ty.as_ref(), Type::Path(ty)
        if ty.path.segments.last().is_some_and(|segment| segment.ident == "Result"))
}

#[derive(Clone)]
struct Classification {
    scope: String,
    kind: String,
}

impl Classification {
    fn resolve(mut self, package: &str) -> Self {
        if self.scope == "pkg" {
            self.kind = package.to_owned();
        }
        self
    }
}

fn classification(function: &ItemFn) -> Option<Classification> {
    let attribute = function.attrs.iter().find(|attribute| {
        attribute
            .path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "classify")
    })?;
    let syn::Meta::List(list) = &attribute.meta else {
        return None;
    };
    let text = list.tokens.to_string();
    if text.trim() == "pkg" {
        return Some(Classification {
            scope: "pkg".into(),
            kind: String::new(),
        });
    }
    let (scope, kind) = text.split_once('=')?;
    let scope = scope.trim().trim_start_matches("r#");
    let kind = kind.trim().trim_matches('"');
    (matches!(scope, "root" | "domain" | "pkg" | "struct") && !kind.is_empty()).then(|| Classification {
        scope: scope.into(),
        kind: kind.into(),
    })
}

#[derive(Default)]
struct Dependencies {
    names: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for Dependencies {
    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Expr::Path(function) = call.func.as_ref() {
            self.names.insert(path_name(&function.path));
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.names.insert(format!(".{}", call.method));
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_macro(&mut self, expression: &'ast ExprMacro) {
        self.names.insert(format!("{}!", path_name(&expression.mac.path)));
        syn::visit::visit_expr_macro(self, expression);
    }
}

fn path_name(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

#[cfg(test)]
#[path = "test.rs"]
mod tests;
