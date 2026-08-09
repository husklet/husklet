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
    // The finding owns the subject it reports.
    #[allow(clippy::needless_pass_by_value)]
    fn inspect(&mut self, name: String, span: Span, block: &syn::Block) {
        // The body's own level is statement position: nothing encloses it that could be producing
        // a value from it.
        let mut depth = Depth {
            statement: true,
            ..Depth::default()
        };
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
    statement: bool,
}

impl Depth {
    /// Reports whether the branch being visited stands as a statement rather than producing a value.
    fn statement(&mut self) -> bool {
        std::mem::replace(&mut self.statement, false)
    }

    /// Visits an else branch, keeping an `else if` chain on the level of the `if` it continues.
    ///
    /// An `else { .. }` block stands wherever its `if` stands, so it carries the same position.
    fn visit_else(&mut self, expression: &syn::ExprIf, position: bool) {
        if let Some((_, branch)) = &expression.else_branch {
            self.statement = position && !matches!(branch.as_ref(), Expr::If(_));
            <Self as Visit>::visit_expr(self, branch);
        }
    }

    /// Deepens the level without ever standing as the deepest construct itself.
    ///
    /// A braced closure body carries the control flow written inside it, so it must raise the
    /// level for that flow. On its own it branches nothing: `spawn_blocking(move || { .. })`
    /// and `map(|row| { .. })` are call arguments, and charging them a level made a
    /// straight-line block illegal wherever it appeared under two real branches.
    fn enter_carrier(&mut self, visit: impl FnOnce(&mut Self)) {
        self.current += 1;
        visit(self);
        self.current -= 1;
    }

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
    fn visit_block(&mut self, block: &'ast syn::Block) {
        // A block stands where its caller put it, and its trailing expression stands there too:
        // the tail of `let value = match .. { arm => { .. if a { x } else { y } } }` produces the
        // arm's value exactly as the unbraced form does. Every other statement is a statement.
        let position = self.statement;
        let tail = block.stmts.len().saturating_sub(1);
        for (index, statement) in block.stmts.iter().enumerate() {
            if let syn::Stmt::Expr(expression, semicolon) = statement {
                self.statement = semicolon.is_some() || index != tail || position;
                self.visit_expr(expression);
                self.statement = false;
            } else {
                // A `let` initializer, an item, or a macro statement is never a branch position.
                self.statement = false;
                self.visit_stmt(statement);
            }
        }
        self.statement = false;
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        let branch = self.statement();
        if let Some((_, guard)) = &arm.guard {
            self.visit_expr(guard);
        }
        self.statement = branch;
        self.visit_expr(&arm.body);
        self.statement = false;
    }

    fn visit_expr(&mut self, expression: &'ast Expr) {
        if !matches!(expression, Expr::If(_) | Expr::Match(_) | Expr::Block(_)) {
            self.statement = false;
        }
        syn::visit::visit_expr(self, expression);
    }

    fn visit_expr_if(&mut self, expression: &'ast syn::ExprIf) {
        let branch = self.statement();
        self.visit_expr(&expression.cond);
        if !branch || guard_clause(expression) {
            self.statement = branch;
            self.visit_block(&expression.then_branch);
            self.visit_else(expression, branch);
            return;
        }
        self.enter("if", expression.if_token.span, |visitor| {
            visitor.statement = true;
            visitor.visit_block(&expression.then_branch);
            visitor.visit_else(expression, true);
        });
    }

    fn visit_expr_match(&mut self, expression: &'ast syn::ExprMatch) {
        let branch = self.statement();
        self.visit_expr(&expression.expr);
        if !branch {
            for arm in &expression.arms {
                self.visit_arm(arm);
            }
            return;
        }
        self.enter("match", expression.match_token.span, |visitor| {
            for arm in &expression.arms {
                visitor.statement = true;
                visitor.visit_arm(arm);
            }
        });
    }

    fn visit_expr_for_loop(&mut self, expression: &'ast syn::ExprForLoop) {
        self.visit_expr(&expression.expr);
        self.enter("for", expression.for_token.span, |visitor| {
            visitor.statement = true;
            visitor.visit_block(&expression.body);
        });
    }

    fn visit_expr_while(&mut self, expression: &'ast syn::ExprWhile) {
        self.visit_expr(&expression.cond);
        self.enter("while", expression.while_token.span, |visitor| {
            visitor.statement = true;
            visitor.visit_block(&expression.body);
        });
    }

    fn visit_expr_loop(&mut self, expression: &'ast syn::ExprLoop) {
        self.enter("loop", expression.loop_token.span, |visitor| {
            visitor.statement = true;
            syn::visit::visit_expr_loop(visitor, expression);
        });
    }

    fn visit_expr_closure(&mut self, expression: &'ast syn::ExprClosure) {
        if !matches!(expression.body.as_ref(), Expr::Block(_)) {
            syn::visit::visit_expr_closure(self, expression);
            return;
        }
        self.enter_carrier(|visitor| {
            visitor.statement = true;
            syn::visit::visit_expr_closure(visitor, expression);
        });
    }

    fn visit_expr_async(&mut self, expression: &'ast syn::ExprAsync) {
        self.enter("async block", expression.async_token.span, |visitor| {
            visitor.statement = true;
            syn::visit::visit_expr_async(visitor, expression);
        });
    }
}

/// Reports whether every path through the branch leaves the enclosing block, which is a guard
/// clause rather than a level of nesting.
fn guard_clause(expression: &syn::ExprIf) -> bool {
    diverging_block(&expression.then_branch)
        && expression
            .else_branch
            .as_ref()
            .is_none_or(|(_, branch)| diverging(branch))
}

fn diverging_block(block: &syn::Block) -> bool {
    match block.stmts.last() {
        Some(syn::Stmt::Expr(expression, _)) => diverging(expression),
        Some(syn::Stmt::Macro(statement)) => diverging_macro(&statement.mac),
        _ => false,
    }
}

fn diverging(expression: &Expr) -> bool {
    match expression {
        Expr::Return(_) | Expr::Break(_) | Expr::Continue(_) => true,
        Expr::Block(block) => diverging_block(&block.block),
        Expr::If(inner) => {
            diverging_block(&inner.then_branch)
                && inner.else_branch.as_ref().is_some_and(|(_, branch)| diverging(branch))
        }
        Expr::Match(inner) => !inner.arms.is_empty() && inner.arms.iter().all(|arm| diverging(&arm.body)),
        Expr::Macro(inner) => diverging_macro(&inner.mac),
        _ => false,
    }
}

fn diverging_macro(invocation: &syn::Macro) -> bool {
    invocation.path.segments.last().is_some_and(|segment| {
        matches!(
            segment.ident.to_string().as_str(),
            "panic" | "unreachable" | "todo" | "unimplemented"
        )
    })
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
    fn accepts_a_branch_that_produces_a_bound_value() {
        let findings = findings(
            r"fn round(control: u16, truncate: bool, value: f64) -> f64 {
    for _ in 0..2 {
        let rounded = if truncate {
            value.trunc()
        } else {
            match control >> 10 & 3 {
                0 => value.round(),
                _ => value.floor(),
            }
        };
        if rounded > value {
            return rounded;
        }
    }
    value
}",
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn reports_a_branch_that_stands_as_a_statement() {
        let findings = findings(
            r"fn drain(rows: &[u8], other: &[u8]) {
    for row in rows {
        for column in other {
            if row > column {
                println!();
            }
        }
    }
}",
        );

        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("depth 3"));
    }

    #[test]
    fn accepts_a_branch_inside_an_arm_of_a_value_match() {
        let findings = findings(
            r"fn width(kind: u8, wide: bool) -> u8 {
    for _ in 0..2 {
        let size = match kind {
            0 => if wide { 8 } else { 4 },
            _ => 1,
        };
        if size > 4 {
            return size;
        }
    }
    0
}",
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn keeps_an_else_if_chain_on_one_level() {
        let findings = findings(
            r"fn select(first: u8, second: u8, third: u8) {
    for _ in 0..2 {
        if first > 0 {
            println!();
        } else if second > 0 {
            println!();
        } else if third > 0 {
            println!();
        }
    }
}",
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn accepts_a_guard_clause_that_leaves_the_enclosing_block() {
        let findings = findings(
            r#"fn resolve(components: &[u8]) -> Result<u8, &'static str> {
    let mut resolved = vec![];
    for component in components {
        match component {
            0 => {}
            1 => {
                if resolved.pop().is_none() {
                    return Err("escapes the root");
                }
            }
            other => resolved.push(*other),
        }
    }
    Ok(resolved.len() as u8)
}"#,
        );

        assert!(findings.is_empty(), "{:?}", findings[0].message);
    }

    #[test]
    fn accepts_guards_that_continue_break_or_panic() {
        for tail in ["continue", "break", "panic!()"] {
            let findings = findings(&format!(
                "fn walk(rows: &[u8], other: &[u8]) {{
    for row in rows {{
        for column in other {{
            if row > column {{
                {tail};
            }}
            println!();
        }}
    }}
}}"
            ));

            assert!(findings.is_empty(), "{tail} was charged a level");
        }
    }

    #[test]
    fn accepts_a_guard_whose_else_also_leaves_the_block() {
        let findings = findings(
            r"fn pick(rows: &[u8], other: &[u8]) -> u8 {
    for row in rows {
        for column in other {
            if row > column {
                return *row;
            } else {
                return *column;
            }
        }
    }
    0
}",
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn reports_a_branch_that_rejoins_the_enclosing_block() {
        let findings = findings(
            r"fn tally(rows: &[u8], other: &[u8]) -> u8 {
    let mut total = 0;
    for row in rows {
        for column in other {
            if row > column {
                total += 1;
            }
        }
    }
    total
}",
        );

        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("depth 3"));
    }

    #[test]
    fn reports_nesting_inside_a_guard_body() {
        let findings = findings(
            r"fn scan(rows: &[u8], other: &[u8]) -> u8 {
    for row in rows {
        if row > &0 {
            for column in other {
                for cell in other {
                    if cell > column {
                        println!();
                    }
                }
            }
            return *row;
        }
    }
    0
}",
        );

        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("depth 4"), "{}", findings[0].message);
    }

    #[test]
    fn accepts_a_straight_line_braced_closure_under_two_branches() {
        let findings = findings(
            r#"fn record(rows: &[u8]) {
    for row in rows {
        if row > &0 {
            rows.iter().for_each(|other| {
                let label = format!("{other}");
                println!("{label}");
            });
        }
    }
}"#,
        );

        assert!(findings.is_empty(), "{:?}", findings.first().map(|f| &f.message));
    }

    #[test]
    fn still_charges_the_closure_that_carries_a_third_branch() {
        let findings = findings(
            r#"fn record(rows: &[u8]) {
    for row in rows {
        if row > &0 {
            rows.iter().for_each(|other| {
                if other > row {
                    println!("{other}");
                }
            });
        }
    }
}"#,
        );

        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("depth 4"), "{}", findings[0].message);
    }

    #[test]
    fn accepts_a_braced_arm_whose_tail_produces_the_value() {
        let findings = findings(
            r"fn shift(kind: u8, wide: bool) -> u64 {
    let mut total = 0;
    for half in 0..2 {
        for offset in 0..4 {
            let size = match kind {
                0 => {
                    let doubled = wide;
                    if doubled { 8 } else { 4 }
                }
                _ => 1,
            };
            total += size + half + offset;
        }
    }
    total
}",
        );

        assert!(findings.is_empty(), "{:?}", findings.first().map(|f| &f.message));
    }

    #[test]
    fn accepts_a_braced_else_that_produces_the_value() {
        let findings = findings(
            r"fn shift(kind: u8, wide: bool) -> u64 {
    let mut total = 0;
    for half in 0..2 {
        for offset in 0..4 {
            let size = if kind == 0 {
                1
            } else {
                let doubled = wide;
                if doubled { 8 } else { 4 }
            };
            total += size + half + offset;
        }
    }
    total
}",
        );

        assert!(findings.is_empty(), "{:?}", findings.first().map(|f| &f.message));
    }

    #[test]
    fn still_reports_a_statement_before_the_tail_of_a_value_block() {
        let findings = findings(
            r"fn shift(kind: u8, wide: bool) -> u64 {
    let mut total = 0;
    for half in 0..2 {
        for offset in 0..4 {
            let size = match kind {
                0 => {
                    if wide {
                        total += 1;
                    }
                    4
                }
                _ => 1,
            };
            total += size + half + offset;
        }
    }
    total
}",
        );

        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("depth 3"), "{}", findings[0].message);
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
