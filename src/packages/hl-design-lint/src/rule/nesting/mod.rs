use proc_macro2::Span;
use syn::{Expr, ImplItemFn, ItemFn, spanned::Spanned, visit::Visit};

use crate::{
    Result,
    model::{Finding, Severity},
    rule::Rule,
    source::{Source, Workspace},
};

/// Reports functions whose structural control-flow depth reaches three.
pub struct DeepControlFlow;

impl Rule for DeepControlFlow {
    fn id(&self) -> &'static str {
        "deep-control-flow"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for source in workspace.sources() {
            let mut functions = Functions {
                source,
                findings: &mut findings,
            };
            functions.visit_file(&source.syntax);
        }
        Ok(findings)
    }
}

struct Functions<'a> {
    source: &'a Source,
    findings: &'a mut Vec<Finding>,
}

impl Functions<'_> {
    fn inspect(&mut self, name: String, span: Span, block: &syn::Block) {
        let mut depth = Depth::default();
        depth.visit_block(block);
        if depth.maximum < 3 {
            return;
        }
        let deepest = depth.span.expect("maximum depth has a span");
        let mut finding = Finding::warning("deep-control-flow", name.clone(), self.source.location(deepest));
        finding.message = format!(
            "`{name}` reaches control-flow depth {} at {}",
            depth.maximum, depth.construct
        );
        finding.help = "use early returns, extract receiver behavior, or model the nested state".to_owned();
        finding.related.push(crate::model::Related {
            label: "function".to_owned(),
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
struct Depth {
    current: usize,
    maximum: usize,
    span: Option<Span>,
    construct: &'static str,
}

impl Depth {
    fn enter(&mut self, construct: &'static str, span: Span, visit: impl FnOnce(&mut Self)) {
        self.current += 1;
        if self.current > self.maximum {
            self.maximum = self.current;
            self.span = Some(span);
            self.construct = construct;
        }
        visit(self);
        self.current -= 1;
    }
}

impl<'ast> Visit<'ast> for Depth {
    fn visit_expr_if(&mut self, expression: &'ast syn::ExprIf) {
        self.enter("if", expression.if_token.span, |visitor| {
            visitor.visit_expr(&expression.cond);
            visitor.visit_block(&expression.then_branch);
            if let Some((_, branch)) = &expression.else_branch {
                if let Expr::If(next) = branch.as_ref() {
                    syn::visit::visit_expr_if(visitor, next);
                } else {
                    visitor.visit_expr(branch);
                }
            }
        });
    }

    fn visit_expr_match(&mut self, expression: &'ast syn::ExprMatch) {
        self.enter("match", expression.match_token.span, |visitor| {
            syn::visit::visit_expr_match(visitor, expression)
        });
    }

    fn visit_expr_for_loop(&mut self, expression: &'ast syn::ExprForLoop) {
        self.enter("for", expression.for_token.span, |visitor| {
            syn::visit::visit_expr_for_loop(visitor, expression)
        });
    }

    fn visit_expr_while(&mut self, expression: &'ast syn::ExprWhile) {
        self.enter("while", expression.while_token.span, |visitor| {
            syn::visit::visit_expr_while(visitor, expression)
        });
    }

    fn visit_expr_loop(&mut self, expression: &'ast syn::ExprLoop) {
        self.enter("loop", expression.loop_token.span, |visitor| {
            syn::visit::visit_expr_loop(visitor, expression)
        });
    }

    fn visit_expr_closure(&mut self, expression: &'ast syn::ExprClosure) {
        self.enter("closure", expression.span(), |visitor| {
            syn::visit::visit_expr_closure(visitor, expression)
        });
    }

    fn visit_expr_async(&mut self, expression: &'ast syn::ExprAsync) {
        self.enter("async block", expression.async_token.span, |visitor| {
            syn::visit::visit_expr_async(visitor, expression)
        });
    }
}
