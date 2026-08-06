use std::collections::HashMap;

use syn::{Expr, ImplItemFn, ItemFn, Pat, ReturnType, Stmt, Type, spanned::Spanned, visit::Visit};

use crate::{
    Result,
    model::{Finding, Location, Related, Severity},
    rule::{Rule, syntax::type_name},
    source::{Source, Workspace, requires_test},
};

/// Reports discarded fallible results when syntax proves the expression is a `Result`.
///
/// This rule deliberately does not guess from operation names. Without rustc type
/// checking, it proves fallibility only for explicit `Result::Ok`/`Result::Err`
/// construction and unambiguous workspace declarations whose written return type
/// ends in `Result`. Method calls are proven only for `self.method()` within its
/// defining impl or a `Type::method()`/`Self::method()` path whose impl owner is
/// syntactically known. Other receiver type resolution is outside this rule.
pub struct IgnoredResult;

impl Rule for IgnoredResult {
    fn id(&self) -> &'static str {
        "ignored-fallible-result"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let signatures = Signatures::collect(workspace);
        let mut findings = Vec::new();
        for source in workspace.production() {
            let mut visitor = Discards {
                source,
                signatures: &signatures,
                findings: &mut findings,
                test_scope: false,
                owner: None,
            };
            visitor.visit_file(&source.syntax);
        }
        Ok(findings)
    }
}

#[derive(Default)]
struct Signatures {
    functions: HashMap<String, Declaration>,
    methods: HashMap<(String, String), Declaration>,
}

#[derive(Clone)]
enum Declaration {
    Result(Location),
    Ambiguous,
}

impl Signatures {
    fn collect(workspace: &Workspace) -> Self {
        let mut signatures = Self::default();
        for source in workspace.production() {
            let mut collector = SignatureCollector {
                signatures: &mut signatures,
                source,
                test_scope: false,
                owner: None,
            };
            collector.visit_file(&source.syntax);
        }
        signatures
    }

    fn insert(map: &mut HashMap<String, Declaration>, name: String, result: bool, location: Location) {
        let declaration = result.then_some(Declaration::Result(location));
        match (map.get(&name), declaration) {
            (None, Some(declaration)) => {
                map.insert(name, declaration);
            }
            (None, None) => {
                map.insert(name, Declaration::Ambiguous);
            }
            (Some(Declaration::Result(_)), Some(_)) => {}
            (Some(Declaration::Ambiguous), _) | (Some(Declaration::Result(_)), None) => {
                map.insert(name, Declaration::Ambiguous);
            }
        }
    }

    fn function(&self, name: &str) -> Option<Location> {
        span(self.functions.get(name))
    }

    fn method(&self, owner: &str, name: &str) -> Option<Location> {
        span(self.methods.get(&(owner.to_owned(), name.to_owned())))
    }
}

fn span(declaration: Option<&Declaration>) -> Option<Location> {
    match declaration {
        Some(Declaration::Result(location)) => Some(location.clone()),
        Some(Declaration::Ambiguous) | None => None,
    }
}

struct SignatureCollector<'a> {
    signatures: &'a mut Signatures,
    source: &'a Source,
    test_scope: bool,
    owner: Option<String>,
}

impl<'ast> Visit<'ast> for SignatureCollector<'_> {
    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        let previous = self.test_scope;
        self.test_scope |= is_test(&function.attrs);
        if !self.test_scope {
            Signatures::insert(
                &mut self.signatures.functions,
                function.sig.ident.to_string(),
                returns_result(&function.sig.output),
                self.source.location(function.sig.output.span()),
            );
        }
        syn::visit::visit_item_fn(self, function);
        self.test_scope = previous;
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        let previous = self.test_scope;
        self.test_scope |= is_test(&function.attrs);
        if !self.test_scope && self.owner.is_some() {
            let key = (self.owner.clone().unwrap_or_default(), function.sig.ident.to_string());
            insert_declaration(
                &mut self.signatures.methods,
                key,
                returns_result(&function.sig.output),
                self.source.location(function.sig.output.span()),
            );
        }
        syn::visit::visit_impl_item_fn(self, function);
        self.test_scope = previous;
    }

    fn visit_item_impl(&mut self, implementation: &'ast syn::ItemImpl) {
        let previous = self.owner.clone();
        self.owner = type_name(&implementation.self_ty);
        syn::visit::visit_item_impl(self, implementation);
        self.owner = previous;
    }

    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        let previous = self.test_scope;
        self.test_scope |= is_test(&module.attrs);
        syn::visit::visit_item_mod(self, module);
        self.test_scope = previous;
    }
}

fn returns_result(output: &ReturnType) -> bool {
    match output {
        ReturnType::Default => false,
        ReturnType::Type(_, ty) => is_result(ty),
    }
}

fn is_result(ty: &Type) -> bool {
    match ty {
        Type::Path(path) => path.path.segments.last().is_some_and(|part| part.ident == "Result"),
        Type::Group(group) => is_result(&group.elem),
        Type::Paren(paren) => is_result(&paren.elem),
        _ => false,
    }
}

struct Discards<'a> {
    source: &'a Source,
    signatures: &'a Signatures,
    findings: &'a mut Vec<Finding>,
    test_scope: bool,
    owner: Option<String>,
}

impl Discards<'_> {
    fn inspect(&mut self, expression: &Expr, kind: &'static str) -> bool {
        let Some(proof) = self.prove(expression) else {
            return false;
        };
        let location = self.source.location(expression.span());
        let subject = location.source.clone();
        let mut finding = Finding::error("ignored-fallible-result", subject.clone(), location);
        finding.message = format!("{kind} discards proven fallible result `{subject}`");
        finding.help = "propagate with `?`, handle every outcome, or log the error with actionable context".into();
        if let Proof::Declaration(location) = proof {
            finding.related.push(Related {
                label: "workspace declaration proves this returns Result".into(),
                location,
            });
        }
        self.findings.push(finding);
        true
    }

    fn prove(&self, expression: &Expr) -> Option<Proof> {
        match expression {
            Expr::Await(awaited) => self.prove(&awaited.base),
            Expr::Group(group) => self.prove(&group.expr),
            Expr::Paren(paren) => self.prove(&paren.expr),
            Expr::Call(call) if explicit_result_constructor(call) => Some(Proof::Explicit),
            Expr::Call(call) => {
                let Expr::Path(path) = call.func.as_ref() else {
                    return None;
                };
                let segments = path.path.segments.iter().collect::<Vec<_>>();
                let name = segments.last()?.ident.to_string();
                match segments.iter().rev().nth(1) {
                    Some(segment) if segment.ident == "Self" => self
                        .signatures
                        .method(self.owner.as_deref()?, &name)
                        .map(Proof::Declaration),
                    Some(segment) => self
                        .signatures
                        .method(&segment.ident.to_string(), &name)
                        .map(Proof::Declaration),
                    None => self.signatures.function(&name).map(Proof::Declaration),
                }
            }
            Expr::MethodCall(call) if matches!(call.receiver.as_ref(), Expr::Path(path) if path.path.is_ident("self")) => {
                self.signatures
                    .method(self.owner.as_deref()?, &call.method.to_string())
                    .map(Proof::Declaration)
            }
            _ => None,
        }
    }

    fn erased_result<'a>(&self, expression: &'a Expr) -> Option<&'a Expr> {
        let Expr::MethodCall(call) = expression else {
            return None;
        };
        matches!(call.method.to_string().as_str(), "ok" | "err")
            .then(|| call.receiver.as_ref())
            .filter(|receiver| self.prove(receiver).is_some())
    }
}

#[derive(Clone)]
enum Proof {
    Explicit,
    Declaration(Location),
}

impl<'ast> Visit<'ast> for Discards<'_> {
    fn visit_stmt(&mut self, statement: &'ast Stmt) {
        if self.test_scope {
            return;
        }
        match statement {
            Stmt::Local(local)
                if matches!(local.pat, Pat::Wild(_))
                    && local
                        .init
                        .as_ref()
                        .is_some_and(|init| self.inspect(&init.expr, "`let _ =`")) =>
            {
                return;
            }
            Stmt::Expr(expression, Some(_)) => {
                if let Some(receiver) = self.erased_result(expression) {
                    self.inspect(receiver, "`.ok()`/`.err()`");
                    return;
                }
                if drop_argument(expression).is_some_and(|value| self.inspect(value, "`drop(...)`")) {
                    return;
                }
                if self.inspect(expression, "expression statement") {
                    return;
                }
            }
            _ => {}
        }
        syn::visit::visit_stmt(self, statement);
    }

    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        self.scoped(is_test(&function.attrs), |visitor| {
            syn::visit::visit_item_fn(visitor, function);
        });
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        self.scoped(is_test(&function.attrs), |visitor| {
            syn::visit::visit_impl_item_fn(visitor, function);
        });
    }

    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        self.scoped(is_test(&module.attrs), |visitor| {
            syn::visit::visit_item_mod(visitor, module);
        });
    }

    fn visit_item_impl(&mut self, implementation: &'ast syn::ItemImpl) {
        let previous = self.owner.clone();
        self.owner = type_name(&implementation.self_ty);
        syn::visit::visit_item_impl(self, implementation);
        self.owner = previous;
    }
}

impl Discards<'_> {
    fn scoped(&mut self, test: bool, visit: impl FnOnce(&mut Self)) {
        let previous = self.test_scope;
        self.test_scope |= test;
        visit(self);
        self.test_scope = previous;
    }
}

fn explicit_result_constructor(call: &syn::ExprCall) -> bool {
    let Expr::Path(path) = call.func.as_ref() else {
        return false;
    };
    let names = path
        .path
        .segments
        .iter()
        .map(|part| part.ident.to_string())
        .collect::<Vec<_>>();
    names.len() >= 2
        && names[names.len() - 2] == "Result"
        && matches!(names.last().map(String::as_str), Some("Ok" | "Err"))
}

fn drop_argument(expression: &Expr) -> Option<&Expr> {
    let Expr::Call(call) = expression else {
        return None;
    };
    let Expr::Path(path) = call.func.as_ref() else {
        return None;
    };
    (path.path.segments.last()?.ident == "drop" && call.args.len() == 1)
        .then(|| call.args.first())
        .flatten()
}

fn is_test(attributes: &[syn::Attribute]) -> bool {
    requires_test(attributes) || attributes.iter().any(|attribute| attribute.path().is_ident("test"))
}

fn insert_declaration<K: Eq + std::hash::Hash>(
    map: &mut HashMap<K, Declaration>,
    key: K,
    result: bool,
    location: Location,
) {
    let declaration = result.then_some(Declaration::Result(location));
    match (map.get(&key), declaration) {
        (None, Some(declaration)) => {
            map.insert(key, declaration);
        }
        (None, None) => {
            map.insert(key, Declaration::Ambiguous);
        }
        (Some(Declaration::Result(_)), Some(_)) => {}
        (Some(Declaration::Ambiguous), _) | (Some(Declaration::Result(_)), None) => {
            map.insert(key, Declaration::Ambiguous);
        }
    }
}

#[cfg(test)]
#[path = "test.rs"]
mod tests;
