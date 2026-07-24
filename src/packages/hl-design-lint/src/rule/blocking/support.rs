use std::collections::HashMap;

use syn::{visit::Visit, Expr, ExprCall, Stmt, Type, UseTree};

use crate::source::requires_test;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum LockKind {
    Blocking,
    Async,
}

pub(super) fn test_attributes(attributes: &[syn::Attribute]) -> bool {
    requires_test(attributes)
        || attributes
            .iter()
            .any(|attribute| attribute.path().is_ident("test"))
}

pub(super) fn blocking_fs_function(name: &str) -> bool {
    matches!(
        name,
        "canonicalize"
            | "copy"
            | "create_dir"
            | "create_dir_all"
            | "hard_link"
            | "metadata"
            | "read"
            | "read_dir"
            | "read_link"
            | "read_to_string"
            | "remove_dir"
            | "remove_dir_all"
            | "remove_file"
            | "rename"
            | "set_permissions"
            | "soft_link"
            | "symlink_metadata"
            | "write"
    )
}

pub(super) fn collect_use(
    tree: &UseTree,
    prefix: Vec<String>,
    aliases: &mut HashMap<String, Vec<String>>,
) {
    match tree {
        UseTree::Path(path) => {
            let mut prefix = prefix;
            prefix.push(path.ident.to_string());
            collect_use(&path.tree, prefix, aliases);
        }
        UseTree::Name(name) => {
            let mut path = prefix;
            path.push(name.ident.to_string());
            aliases.insert(name.ident.to_string(), path);
        }
        UseTree::Rename(rename) => {
            let mut path = prefix;
            path.push(rename.ident.to_string());
            aliases.insert(rename.rename.to_string(), path);
        }
        UseTree::Group(group) => {
            for tree in &group.items {
                collect_use(tree, prefix.clone(), aliases);
            }
        }
        UseTree::Glob(_) => {}
    }
}

pub(super) fn expression_path(expression: &Expr) -> Option<&syn::Path> {
    let Expr::Path(path) = expression else {
        return None;
    };
    Some(&path.path)
}

pub(super) fn lock_acquisition(expression: &Expr) -> Option<&syn::ExprMethodCall> {
    let Expr::MethodCall(call) = expression else {
        return None;
    };
    if matches!(call.method.to_string().as_str(), "lock" | "read" | "write") {
        Some(call)
    } else if matches!(call.method.to_string().as_str(), "unwrap" | "expect") {
        lock_acquisition(&call.receiver)
    } else {
        None
    }
}

pub(super) fn command_constructor(
    expression: &Expr,
    resolve: impl Fn(&syn::Path) -> Vec<String> + Copy,
) -> bool {
    let Expr::Call(call) = expression else {
        return false;
    };
    let Expr::Path(function) = call.func.as_ref() else {
        return false;
    };
    resolve(&function.path) == ["std", "process", "Command", "new"]
}

pub(super) fn fs_open_options_constructor(
    expression: &Expr,
    resolve: impl Fn(&syn::Path) -> Vec<String> + Copy,
) -> bool {
    match expression {
        Expr::Call(call) => {
            let Expr::Path(function) = call.func.as_ref() else {
                return false;
            };
            resolve(&function.path) == ["std", "fs", "OpenOptions", "new"]
        }
        Expr::MethodCall(call) => fs_open_options_constructor(&call.receiver, resolve),
        _ => false,
    }
}

pub(super) fn lock_constructor(
    expression: &Expr,
    resolve: impl Fn(&syn::Path) -> Vec<String> + Copy,
) -> Option<LockKind> {
    let Expr::Call(call) = expression else {
        return None;
    };
    let Expr::Path(function) = call.func.as_ref() else {
        return None;
    };
    lock_path(&resolve(&function.path))
}

pub(super) fn lock_type(
    ty: &Type,
    resolve: impl Fn(&syn::Path) -> Vec<String> + Copy,
) -> Option<LockKind> {
    match ty {
        Type::Path(path) => lock_path(&resolve(&path.path)),
        Type::Reference(reference) => lock_type(&reference.elem, resolve),
        _ => None,
    }
}

fn lock_path(path: &[String]) -> Option<LockKind> {
    let names = path.iter().map(String::as_str).collect::<Vec<_>>();
    if matches!(
        names.as_slice(),
        ["std", "sync", "Mutex" | "RwLock", ..] | ["parking_lot", "Mutex" | "RwLock", ..]
    ) {
        Some(LockKind::Blocking)
    } else if matches!(
        names.as_slice(),
        ["tokio", "sync", "Mutex" | "RwLock", ..] | ["tokio", "sync", "mpsc", "Receiver", ..]
    ) {
        Some(LockKind::Async)
    } else {
        None
    }
}

pub(super) fn call_is_blocking_boundary(
    call: &ExprCall,
    resolve: impl Fn(&syn::Path) -> Vec<String>,
) -> bool {
    let Expr::Path(function) = call.func.as_ref() else {
        return false;
    };
    let path = resolve(&function.path);
    path == ["tokio", "task", "spawn_blocking"] || path == ["tokio", "task", "block_in_place"]
}

pub(super) fn contains_await(statement: &Stmt) -> bool {
    struct Await(bool);
    impl<'ast> Visit<'ast> for Await {
        fn visit_expr_await(&mut self, _: &'ast syn::ExprAwait) {
            self.0 = true;
        }
    }
    let mut visitor = Await(false);
    visitor.visit_stmt(statement);
    visitor.0
}

pub(super) fn references(statement: &Stmt, name: &str) -> bool {
    struct Reference<'a> {
        name: &'a str,
        found: bool,
    }
    impl<'ast> Visit<'ast> for Reference<'_> {
        fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
            self.found |= path.path.is_ident(self.name);
        }
    }
    let mut visitor = Reference { name, found: false };
    visitor.visit_stmt(statement);
    visitor.found
}

pub(super) fn drops(statement: &Stmt, name: &str) -> bool {
    let Stmt::Expr(Expr::Call(call), _) = statement else {
        return false;
    };
    let Expr::Path(function) = call.func.as_ref() else {
        return false;
    };
    function.path.is_ident("drop")
        && call
            .args
            .first()
            .and_then(expression_path)
            .is_some_and(|path| path.is_ident(name))
}
