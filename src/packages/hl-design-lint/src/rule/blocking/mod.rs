use std::collections::{HashMap, HashSet};

use syn::{
    Block, Expr, ExprCall, ExprClosure, ExprMethodCall, ImplItemFn, ItemFn, ItemMod, ItemUse, Local, Pat, Stmt,
    spanned::Spanned, visit::Visit,
};

use crate::{
    Result,
    model::{Finding, Related, Severity},
    rule::Rule,
    source::{Source, Workspace, requires_test},
};

mod support;
use support::*;

/// Rejects proven executor-blocking work and synchronous guards across suspension points.
pub struct AsyncBlocking;

impl Rule for AsyncBlocking {
    fn id(&self) -> &'static str {
        "async-blocking-operation"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for source in workspace.sources() {
            let mut visitor = Blocking::new(source);
            visitor.visit_file(&source.syntax);
            findings.extend(visitor.findings);
        }
        Ok(findings)
    }
}

struct Blocking<'a> {
    source: &'a Source,
    aliases: HashMap<String, Vec<String>>,
    async_depth: usize,
    exempt_depth: usize,
    test_depth: usize,
    command_bindings: Vec<HashSet<String>>,
    lock_bindings: Vec<HashMap<String, LockKind>>,
    findings: Vec<Finding>,
}

impl<'a> Blocking<'a> {
    fn new(source: &'a Source) -> Self {
        Self {
            source,
            aliases: HashMap::new(),
            async_depth: 0,
            exempt_depth: 0,
            test_depth: 0,
            command_bindings: vec![HashSet::new()],
            lock_bindings: vec![HashMap::new()],
            findings: Vec::new(),
        }
    }

    fn active(&self) -> bool {
        !self.source.test && self.test_depth == 0 && self.async_depth > 0 && self.exempt_depth == 0
    }

    fn resolve(&self, path: &syn::Path) -> Vec<String> {
        let mut parts = path
            .segments
            .iter()
            .map(|part| part.ident.to_string())
            .collect::<Vec<_>>();
        if let Some(prefix) = parts.first().and_then(|first| self.aliases.get(first)) {
            parts.splice(0..1, prefix.clone());
        }
        parts
    }

    fn report(&mut self, span: proc_macro2::Span, kind: &str, subject: String, help: &str) {
        let location = self.source.location(span);
        let mut finding = Finding::error("async-blocking-operation", subject.clone(), location.clone());
        finding.message = format!("{kind} `{subject}` can block the async executor thread");
        finding.help = help.to_owned();
        finding.related.push(Related {
            label: format!(
                "proven inside an async lexical scope in crate `{}`",
                self.source.package
            ),
            location,
        });
        self.findings.push(finding);
    }

    fn path_call(&mut self, call: &ExprCall) {
        let Expr::Path(function) = call.func.as_ref() else {
            return;
        };
        let path = self.resolve(&function.path);
        if path == ["std", "thread", "sleep"] {
            self.report(
                call.span(),
                "blocking sleep",
                path.join("::"),
                "use an async timer such as `tokio::time::sleep(...).await`",
            );
        } else if path.first().is_some_and(|part| part == "std")
            && path.get(1).is_some_and(|part| part == "fs")
            && (path.get(2).is_some_and(|name| fs_function(name))
                || matches!(
                    path.as_slice(),
                    [_, _, kind, operation]
                        if kind == "File" && matches!(operation.as_str(), "open" | "create")
                ))
        {
            self.report(
                call.span(),
                "synchronous filesystem operation",
                path.join("::"),
                "use the runtime's async filesystem API or isolate the complete operation with `spawn_blocking`",
            );
        }
    }

    fn method_call(&mut self, call: &ExprMethodCall) {
        let method = call.method.to_string();
        if matches!(method.as_str(), "blocking_recv" | "blocking_lock") && self.proven_tokio_receiver(&call.receiver) {
            self.report(
                call.span(),
                "explicit blocking runtime operation",
                method,
                "use the corresponding asynchronous operation and await it",
            );
            return;
        }
        if matches!(
            method.as_str(),
            "spawn" | "status" | "output" | "wait" | "wait_with_output"
        ) && self.command_receiver(&call.receiver)
        {
            self.report(
                call.span(),
                "synchronous process operation",
                format!("std::process::Command::{method}"),
                "use `tokio::process::Command` or isolate the complete process lifecycle with `spawn_blocking`",
            );
            return;
        }
        if matches!(method.as_str(), "lock" | "read" | "write")
            && self.lock_receiver(&call.receiver) == Some(LockKind::Blocking)
        {
            self.report(
                call.span(),
                "blocking lock acquisition",
                method,
                "use an async-aware lock when contention may span executor work, or acquire the synchronous lock outside async execution",
            );
        } else if method == "open" && fs_options_constructor(&call.receiver, |path| self.resolve(path)) {
            self.report(
                call.span(),
                "synchronous filesystem operation",
                "std::fs::OpenOptions::open".to_owned(),
                "use `tokio::fs::OpenOptions` or isolate the complete operation with `spawn_blocking`",
            );
        }
    }

    fn command_receiver(&self, expression: &Expr) -> bool {
        if let Expr::Path(path) = expression {
            return path.path.get_ident().is_some_and(|name| {
                self.command_bindings
                    .iter()
                    .rev()
                    .any(|scope| scope.contains(&name.to_string()))
            });
        }
        command_constructor(expression, |path| self.resolve(path))
    }

    fn lock_receiver(&self, expression: &Expr) -> Option<LockKind> {
        let Expr::Path(path) = expression else {
            return None;
        };
        let name = path.path.get_ident()?.to_string();
        self.lock_bindings
            .iter()
            .rev()
            .find_map(|scope| scope.get(&name).copied())
    }

    fn proven_tokio_receiver(&self, expression: &Expr) -> bool {
        self.lock_receiver(expression) == Some(LockKind::Async)
            || expression_path(expression)
                .map(|path| self.resolve(path))
                .is_some_and(|path| path.starts_with(&["tokio".into(), "sync".into()]))
    }

    fn bind_local(&mut self, local: &Local) {
        let Pat::Ident(binding) = &local.pat else {
            return;
        };
        let name = binding.ident.to_string();
        let Some(initializer) = &local.init else {
            return;
        };
        if command_constructor(&initializer.expr, |path| self.resolve(path)) {
            self.command_bindings.last_mut().unwrap().insert(name.clone());
        }
        if let Some(kind) = lock_constructor(&initializer.expr, |path| self.resolve(path)) {
            self.lock_bindings.last_mut().unwrap().insert(name, kind);
        }
    }

    fn inspect_guard_lifetimes(&mut self, block: &Block) {
        if !self.active() {
            return;
        }
        for (index, statement) in block.stmts.iter().enumerate() {
            let Stmt::Local(local) = statement else {
                continue;
            };
            let Pat::Ident(binding) = &local.pat else {
                continue;
            };
            let Some(initializer) = &local.init else {
                continue;
            };
            let Some(lock) = lock_acquisition(&initializer.expr) else {
                continue;
            };
            if !matches!(lock.method.to_string().as_str(), "lock" | "read" | "write")
                || self.lock_receiver(&lock.receiver) != Some(LockKind::Blocking)
            {
                continue;
            }
            let tail = &block.stmts[index + 1..];
            let Some(await_index) = tail.iter().position(contains_await) else {
                continue;
            };
            let before_await = &tail[..await_index];
            let after_await = &tail[await_index..];
            let name = binding.ident.to_string();
            if before_await.iter().any(|stmt| drops(stmt, &name))
                || !after_await.iter().any(|stmt| references(stmt, &name))
            {
                continue;
            }
            self.report(
                lock.span(),
                "synchronous lock guard held across await",
                name,
                "narrow the guard's lexical lifetime or explicitly drop it before awaiting; use an async-aware lock only when the protected design requires it",
            );
        }
    }
}

impl<'ast> Visit<'ast> for Blocking<'_> {
    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        collect_use(&item.tree, Vec::new(), &mut self.aliases);
    }

    fn visit_item_mod(&mut self, module: &'ast ItemMod) {
        let aliases = self.aliases.clone();
        self.test_depth += usize::from(requires_test(&module.attrs));
        syn::visit::visit_item_mod(self, module);
        self.test_depth -= usize::from(requires_test(&module.attrs));
        self.aliases = aliases;
    }

    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        let test = test_attributes(&function.attrs);
        self.test_depth += usize::from(test);
        self.async_depth += usize::from(function.sig.asyncness.is_some());
        self.bind_arguments(&function.sig.inputs);
        self.inspect_guard_lifetimes(&function.block);
        syn::visit::visit_block(self, &function.block);
        self.pop_function(function.sig.asyncness.is_some(), test);
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        let test = test_attributes(&function.attrs);
        self.test_depth += usize::from(test);
        self.async_depth += usize::from(function.sig.asyncness.is_some());
        self.bind_arguments(&function.sig.inputs);
        self.inspect_guard_lifetimes(&function.block);
        syn::visit::visit_block(self, &function.block);
        self.pop_function(function.sig.asyncness.is_some(), test);
    }

    fn visit_expr_async(&mut self, expression: &'ast syn::ExprAsync) {
        self.async_depth += 1;
        self.inspect_guard_lifetimes(&expression.block);
        syn::visit::visit_block(self, &expression.block);
        self.async_depth -= 1;
    }

    fn visit_expr_closure(&mut self, closure: &'ast ExprClosure) {
        let async_scope = closure.asyncness.is_some();
        self.async_depth += usize::from(async_scope);
        syn::visit::visit_expr_closure(self, closure);
        self.async_depth -= usize::from(async_scope);
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        let exempt = boundary_call(call, |path| self.resolve(path));
        if exempt {
            self.exempt_depth += 1;
            syn::visit::visit_expr_call(self, call);
            self.exempt_depth -= 1;
        } else {
            if self.active() {
                self.path_call(call);
            }
            syn::visit::visit_expr_call(self, call);
        }
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        if self.active() {
            self.method_call(call);
        }
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_block(&mut self, block: &'ast Block) {
        let aliases = self.aliases.clone();
        self.command_bindings.push(HashSet::new());
        self.lock_bindings.push(HashMap::new());
        self.inspect_guard_lifetimes(block);
        syn::visit::visit_block(self, block);
        self.lock_bindings.pop();
        self.command_bindings.pop();
        self.aliases = aliases;
    }

    fn visit_local(&mut self, local: &'ast Local) {
        self.bind_local(local);
        syn::visit::visit_local(self, local);
    }
}

impl Blocking<'_> {
    fn bind_arguments(&mut self, arguments: &syn::punctuated::Punctuated<syn::FnArg, syn::Token![,]>) {
        self.command_bindings.push(HashSet::new());
        self.lock_bindings.push(HashMap::new());
        for argument in arguments {
            let syn::FnArg::Typed(argument) = argument else {
                continue;
            };
            let Pat::Ident(binding) = argument.pat.as_ref() else {
                continue;
            };
            if let Some(kind) = lock_type(&argument.ty, |path| self.resolve(path)) {
                self.lock_bindings
                    .last_mut()
                    .unwrap()
                    .insert(binding.ident.to_string(), kind);
            }
        }
    }

    fn pop_function(&mut self, async_scope: bool, test: bool) {
        self.lock_bindings.pop();
        self.command_bindings.pop();
        self.async_depth -= usize::from(async_scope);
        self.test_depth -= usize::from(test);
    }
}

#[cfg(test)]
#[path = "test.rs"]
mod tests;
