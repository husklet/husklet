use proc_macro2::Span;
use syn::{BinOp, ImplItemFn, ItemFn, Lit, spanned::Spanned, visit::Visit};

use crate::{
    Result,
    model::{Finding, Severity},
    rule::Rule,
    source::{Source, Workspace},
};

/// Rejects hand-written long-option dispatch in executable applications and tools.
pub struct ManualDispatch;

impl Rule for ManualDispatch {
    fn id(&self) -> &'static str {
        "manual-cli-dispatch"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for source in workspace.production().filter(|source| cli_scope(source)) {
            let mut functions = Functions {
                source,
                findings: &mut findings,
            };
            functions.visit_file(&source.syntax);
        }
        Ok(findings)
    }
}

fn cli_scope(source: &Source) -> bool {
    source.domain == "apps"
        || source.path.file_name().is_some_and(|name| name == "main.rs")
        || source.path.components().any(|component| component.as_os_str() == "bin")
}

struct Functions<'a> {
    source: &'a Source,
    findings: &'a mut Vec<Finding>,
}

impl Functions<'_> {
    fn inspect(&mut self, name: String, span: Span, block: &syn::Block) {
        let mut dispatch = Dispatch::default();
        dispatch.visit_block(block);
        let Some(flag) = dispatch.flag else { return };
        if !dispatch.looped || !dispatch.cursor_advanced {
            return;
        }
        let mut finding = Finding::error("manual-cli-dispatch", name, self.source.location(flag));
        finding.message =
            "manual long-option dispatch couples argument traversal, index mutation, and flag policy".into();
        finding.help = "derive a typed CLI parser and let the parsed command/options own validation".into();
        finding.related.push(crate::model::Related {
            label: "manual parser".into(),
            location: self.source.location(span),
        });
        self.findings.push(finding);
    }
}

impl<'ast> Visit<'ast> for Functions<'_> {
    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        self.inspect(function.sig.ident.to_string(), function.span(), &function.block);
        syn::visit::visit_item_fn(self, function);
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        self.inspect(function.sig.ident.to_string(), function.span(), &function.block);
        syn::visit::visit_impl_item_fn(self, function);
    }
}

#[derive(Default)]
struct Dispatch {
    branch_depth: usize,
    looped: bool,
    cursor_advanced: bool,
    flag: Option<Span>,
}

impl Dispatch {
    fn branch(&mut self, visit: impl FnOnce(&mut Self)) {
        self.branch_depth += 1;
        visit(self);
        self.branch_depth -= 1;
    }
}

impl<'ast> Visit<'ast> for Dispatch {
    fn visit_expr_for_loop(&mut self, expression: &'ast syn::ExprForLoop) {
        self.looped = true;
        syn::visit::visit_expr_for_loop(self, expression);
    }

    fn visit_expr_while(&mut self, expression: &'ast syn::ExprWhile) {
        self.looped = true;
        syn::visit::visit_expr_while(self, expression);
    }

    fn visit_expr_loop(&mut self, expression: &'ast syn::ExprLoop) {
        self.looped = true;
        syn::visit::visit_expr_loop(self, expression);
    }

    fn visit_expr_if(&mut self, expression: &'ast syn::ExprIf) {
        self.branch(|visitor| syn::visit::visit_expr_if(visitor, expression));
    }

    fn visit_expr_match(&mut self, expression: &'ast syn::ExprMatch) {
        self.branch(|visitor| syn::visit::visit_expr_match(visitor, expression));
    }

    fn visit_lit(&mut self, literal: &'ast Lit) {
        if self.branch_depth > 0
            && let Lit::Str(value) = literal
            && value.value().starts_with("--")
        {
            self.flag.get_or_insert(value.span());
        }
        syn::visit::visit_lit(self, literal);
    }

    fn visit_expr_binary(&mut self, expression: &'ast syn::ExprBinary) {
        self.cursor_advanced |= matches!(
            expression.op,
            BinOp::AddAssign(_) | BinOp::SubAssign(_) | BinOp::MulAssign(_) | BinOp::DivAssign(_)
        );
        syn::visit::visit_expr_binary(self, expression);
    }

    fn visit_expr_assign(&mut self, expression: &'ast syn::ExprAssign) {
        self.cursor_advanced = true;
        syn::visit::visit_expr_assign(self, expression);
    }

    fn visit_expr_method_call(&mut self, expression: &'ast syn::ExprMethodCall) {
        self.cursor_advanced |= expression.method == "next";
        syn::visit::visit_expr_method_call(self, expression);
    }
}

#[cfg(test)]
mod tests {
    use super::{ManualDispatch, Rule};
    use crate::source::Workspace;
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
    };

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn findings(source: &str) -> Vec<crate::model::Finding> {
        let root = std::env::temp_dir().join(format!(
            "hl-design-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let path = root.join("src/apps/tool/src/main.rs");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            root.join("src/apps/tool/Cargo.toml"),
            "[package]\nname='tool'\nversion='0.0.0'\n",
        )
        .unwrap();
        fs::write(&path, source).unwrap();
        let workspace = Workspace::load([path]).unwrap();
        let findings = ManualDispatch.check(&workspace).unwrap();
        fs::remove_dir_all(root).unwrap();
        findings
    }

    #[test]
    fn rejects_indexed_long_option_dispatch() {
        let findings = findings(
            r#"fn parse(arguments: &[String]) {
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--isa" => index += 2,
            _ => index += 1,
        }
    }
}"#,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].location.line, 5);
        assert_eq!(findings[0].subject, "parse");
    }

    #[test]
    fn accepts_typed_derive_parser() {
        let findings = findings(
            r#"#[derive(clap::Parser)]
struct Options { #[arg(long)] isa: String }
fn parse() -> Options { <Options as clap::Parser>::parse() }"#,
        );
        assert!(findings.is_empty());
    }
}
