use proc_macro2::Span;
use syn::{Expr, ImplItemFn, ItemFn, ItemMod, spanned::Spanned, visit::Visit};

use crate::{
    Result,
    model::{Finding, Severity},
    rule::Rule,
    source::{Source, Workspace, requires_test},
};

const MAXIMUM_NESTING: usize = 2;

/// Rejects functions whose syntactic control-flow nesting exceeds two levels.
pub struct MaximumNesting;

impl Rule for MaximumNesting {
    fn id(&self) -> &'static str {
        "maximum-nesting"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for source in workspace.sources() {
            let mut functions = Functions {
                source,
                findings: &mut findings,
                test_depth: usize::from(source.test),
            };
            functions.visit_file(&source.syntax);
        }
        Ok(findings)
    }
}

struct Functions<'a> {
    source: &'a Source,
    findings: &'a mut Vec<Finding>,
    test_depth: usize,
}

impl Functions<'_> {
    fn inspect(&mut self, name: String, span: Span, block: &syn::Block) {
        let mut depth = Depth::default();
        depth.visit_block(block);
        if depth.maximum <= MAXIMUM_NESTING {
            return;
        }
        let deepest = depth.span.expect("maximum depth has a span");
        let mut finding = Finding::error("maximum-nesting", name.clone(), self.source.location(deepest));
        finding.message = format!(
            "`{name}` reaches syntactic nesting depth {} at {}; the maximum is {MAXIMUM_NESTING}",
            depth.maximum, depth.construct,
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
        let test =
            requires_test(&function.attrs) || function.attrs.iter().any(|attribute| attribute.path().is_ident("test"));
        if self.test_depth == 0 && !test {
            self.inspect(function.sig.ident.to_string(), function.span(), &function.block);
        }
        syn::visit::visit_item_fn(self, function);
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        let test =
            requires_test(&function.attrs) || function.attrs.iter().any(|attribute| attribute.path().is_ident("test"));
        if self.test_depth == 0 && !test {
            self.inspect(function.sig.ident.to_string(), function.span(), &function.block);
        }
        syn::visit::visit_impl_item_fn(self, function);
    }

    fn visit_item_mod(&mut self, module: &'ast ItemMod) {
        let test = requires_test(&module.attrs);
        self.test_depth += usize::from(test);
        syn::visit::visit_item_mod(self, module);
        self.test_depth -= usize::from(test);
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
        self.visit_expr(&expression.cond);
        self.enter("if", expression.if_token.span, |visitor| {
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
        self.visit_expr(&expression.expr);
        self.enter("match", expression.match_token.span, |visitor| {
            for arm in &expression.arms {
                visitor.visit_arm(arm);
            }
        });
    }

    fn visit_expr_for_loop(&mut self, expression: &'ast syn::ExprForLoop) {
        self.visit_expr(&expression.expr);
        self.enter("for", expression.for_token.span, |visitor| {
            visitor.visit_block(&expression.body);
        });
    }

    fn visit_expr_while(&mut self, expression: &'ast syn::ExprWhile) {
        self.visit_expr(&expression.cond);
        self.enter("while", expression.while_token.span, |visitor| {
            visitor.visit_block(&expression.body);
        });
    }

    fn visit_expr_loop(&mut self, expression: &'ast syn::ExprLoop) {
        self.enter("loop", expression.loop_token.span, |visitor| {
            syn::visit::visit_expr_loop(visitor, expression);
        });
    }

    fn visit_expr_closure(&mut self, expression: &'ast syn::ExprClosure) {
        if !matches!(expression.body.as_ref(), Expr::Block(_)) {
            syn::visit::visit_expr_closure(self, expression);
            return;
        }
        self.enter("closure", expression.span(), |visitor| {
            syn::visit::visit_expr_closure(visitor, expression);
        });
    }

    fn visit_expr_async(&mut self, expression: &'ast syn::ExprAsync) {
        self.enter("async block", expression.async_token.span, |visitor| {
            syn::visit::visit_expr_async(visitor, expression);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{MaximumNesting, Rule};
    use crate::source::Workspace;
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
    };

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn findings(source: &str) -> Vec<crate::model::Finding> {
        let root = std::env::temp_dir().join(format!(
            "hl-design-lint-nesting-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='nesting-fixture'\nversion='0.0.0'\n",
        )
        .unwrap();
        let path = root.join("src/lib.rs");
        fs::write(&path, source).unwrap();
        let workspace = Workspace::load([path]).unwrap();
        let findings = MaximumNesting.check(&workspace).unwrap();
        fs::remove_dir_all(root).unwrap();
        findings
    }

    #[test]
    fn reports_nested_benchmark_orchestration_at_the_deepest_construct() {
        let findings = findings(
            r#"fn report(cases: &[u8]) {
    for case in cases {
        for sample in 0..5 {
            match sample {
                0 => println!("{case}"),
                _ => println!("{sample}"),
            }
        }
    }
}"#,
        );

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].subject, "report");
        assert_eq!(findings[0].location.line, 4);
        assert!(findings[0].message.contains("maximum is 2"));
        assert!(findings[0].message.contains("depth 3"));
    }

    #[test]
    fn accepts_flat_match_arms_and_declarative_result_data() {
        let findings = findings(
            r#"struct ResultRow { name: &'static str, samples: &'static [u64] }
fn summarize(row: &ResultRow) {
    match row.samples.first() {
        Some(value) => println!("{} {value}", row.name),
        None => println!("{} has no samples", row.name),
    }
}"#,
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn accepts_predicate_closures_inside_conditions() {
        let findings = findings(
            r#"fn validate(options: &mut Vec<String>) -> bool {
    for name in ["ro", "readonly"] {
        if options.iter().any(|option| option == name) {
            return false;
        }
    }
    true
}"#,
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn reports_control_flow_inside_a_braced_closure_body() {
        let findings = findings(
            r#"fn schedule(cases: &[u8]) {
    for case in cases {
        cases.iter().for_each(|other| {
            if other > case {
                println!("{other}");
            }
        });
    }
}"#,
        );

        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("depth 3"));
    }

    #[test]
    fn accepts_a_scrutinee_call_beside_a_nested_match() {
        let findings = findings(
            r"fn classify(values: &[u8]) -> u8 {
    match values.iter().max_by_key(|value| **value) {
        Some(value) => match value {
            0 => 1,
            other => *other,
        },
        None => 0,
    }
}",
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn ignores_inline_test_harness_nesting() {
        let findings = findings(
            r"#[cfg(test)] mod tests {
    #[test] fn exhaustive_fixture() {
        for a in 0..2 { for b in 0..2 { match (a, b) { _ => {} } } }
    }
}",
        );
        assert!(findings.is_empty());
    }
}
