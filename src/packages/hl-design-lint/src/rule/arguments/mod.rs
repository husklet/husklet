use proc_macro2::Span;
use syn::{BinOp, Expr, ExprCall, FnArg, ImplItemFn, ItemFn, Lit, Pat, Signature, spanned::Spanned, visit::Visit};

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
    fn inspect(&mut self, name: String, span: Span, signature: &Signature, block: &syn::Block) {
        let mut input = CliInput::default();
        input.visit_block(block);
        input.present |= signature.inputs.iter().any(|argument| {
            let FnArg::Typed(argument) = argument else { return false };
            let Pat::Ident(argument) = argument.pat.as_ref() else {
                return false;
            };
            matches!(argument.ident.to_string().as_str(), "args" | "arguments")
        });
        if !input.present {
            return;
        }
        let mut dispatch = Dispatch::default();
        dispatch.visit_block(block);
        let Some(flag) = dispatch.flag else { return };
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
        self.inspect(
            function.sig.ident.to_string(),
            function.span(),
            &function.sig,
            &function.block,
        );
        syn::visit::visit_item_fn(self, function);
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        self.inspect(
            function.sig.ident.to_string(),
            function.span(),
            &function.sig,
            &function.block,
        );
        syn::visit::visit_impl_item_fn(self, function);
    }
}

#[derive(Default)]
struct Dispatch {
    flag: Option<Span>,
}

impl<'ast> Visit<'ast> for Dispatch {
    fn visit_expr_for_loop(&mut self, expression: &'ast syn::ExprForLoop) {
        self.inspect_loop(&expression.body);
        syn::visit::visit_expr_for_loop(self, expression);
    }

    fn visit_expr_while(&mut self, expression: &'ast syn::ExprWhile) {
        self.inspect_loop(&expression.body);
        syn::visit::visit_expr_while(self, expression);
    }

    fn visit_expr_loop(&mut self, expression: &'ast syn::ExprLoop) {
        self.inspect_loop(&expression.body);
        syn::visit::visit_expr_loop(self, expression);
    }
}

impl Dispatch {
    fn inspect_loop(&mut self, block: &syn::Block) {
        let mut region = Region::default();
        region.visit_block(block);
        if region.cursor_advanced {
            self.flag = self.flag.or(region.flag);
        }
    }
}

#[derive(Default)]
struct Region {
    branch_depth: usize,
    cursor_advanced: bool,
    flag: Option<Span>,
}

impl Region {
    fn branch(&mut self, visit: impl FnOnce(&mut Self)) {
        self.branch_depth += 1;
        visit(self);
        self.branch_depth -= 1;
    }
}

impl<'ast> Visit<'ast> for Region {
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
        self.cursor_advanced |= matches!(expression.op, BinOp::AddAssign(_) | BinOp::SubAssign(_))
            && expression_name(&expression.left)
                .is_some_and(|name| matches!(name.as_str(), "index" | "cursor" | "position"));
        syn::visit::visit_expr_binary(self, expression);
    }

    fn visit_expr_method_call(&mut self, expression: &'ast syn::ExprMethodCall) {
        self.cursor_advanced |= expression.method == "next"
            && expression_name(&expression.receiver)
                .is_some_and(|name| matches!(name.as_str(), "args" | "arguments" | "iterator"));
        syn::visit::visit_expr_method_call(self, expression);
    }
}

fn expression_name(expression: &Expr) -> Option<String> {
    let Expr::Path(expression) = expression else {
        return None;
    };
    expression.path.get_ident().map(ToString::to_string)
}

#[derive(Default)]
struct CliInput {
    present: bool,
}

impl<'ast> Visit<'ast> for CliInput {
    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Expr::Path(function) = call.func.as_ref() {
            let segments = function
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>();
            self.present |= segments.ends_with(&["env".into(), "args".into()])
                || segments.ends_with(&["env".into(), "args_os".into()]);
        }
        syn::visit::visit_expr_call(self, call);
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
            r"#[derive(clap::Parser)]
struct Options { #[arg(long)] isa: String }
fn parse() -> Options { <Options as clap::Parser>::parse() }",
        );
        assert!(findings.is_empty());
    }

    #[test]
    fn ignores_unrelated_domain_loop_and_configuration_flag() {
        let findings = findings(
            r#"fn render(arguments: &[String], rows: &mut [String]) {
    for row in rows { *row = row.to_uppercase(); }
    if arguments.is_empty() { println!("--theme"); }
}"#,
        );
        assert!(findings.is_empty());
    }

    #[test]
    fn ignores_guest_argument_passthrough() {
        let findings = findings(
            r"fn launch(arguments: &[String], command: &mut Command) {
    for argument in arguments { command.arg(argument); }
}",
        );
        assert!(findings.is_empty());
    }

    #[test]
    fn ignores_typed_bootstrap_descriptor() {
        let findings = findings(
            r#"struct Bootstrap { descriptor: i32 }
fn launch(arguments: &[String], bootstrap: Bootstrap) {
    for argument in arguments { println!("{argument}"); }
    if bootstrap.descriptor < 0 { println!("--invalid-bootstrap"); }
}"#,
        );
        assert!(findings.is_empty());
    }
}
