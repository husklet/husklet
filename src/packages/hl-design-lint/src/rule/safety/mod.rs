use proc_macro2::Span;
use syn::{ExprUnsafe, ImplItemFn, ItemFn, ItemImpl, ItemMod, ItemTrait, TraitItemFn, spanned::Spanned, visit::Visit};

use crate::{
    Result,
    model::{Finding, Severity},
    rule::Rule,
    source::{Source, Workspace},
};

/// Confines unsafe Rust to reviewed native and FFI boundaries.
pub struct Boundary;

impl Rule for Boundary {
    fn id(&self) -> &'static str {
        "unsafe-boundary"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for source in workspace.sources() {
            let mut visitor = Syntax {
                source,
                boundary: boundary(source),
                application: application(source),
                ffi_depth: 0,
                findings: Vec::new(),
            };
            visitor.visit_file(&source.syntax);
            findings.extend(visitor.findings);
        }
        Ok(findings)
    }
}

struct Syntax<'a> {
    source: &'a Source,
    boundary: bool,
    application: bool,
    ffi_depth: usize,
    findings: Vec<Finding>,
}

impl Syntax<'_> {
    fn allowed(&self) -> bool {
        self.boundary || self.ffi_depth != 0
    }

    fn report(&mut self, span: Span, subject: String, construct: &'static str) {
        if self.allowed() {
            return;
        }
        let mut finding = Finding::error("unsafe-boundary", subject, self.source.location(span));
        finding.message =
            format!("{construct} is outside the native execution source tree or an explicit application FFI module");
        finding.help = "move the operation behind a safe domain-owned port and keep the minimum unsafe adapter in an approved boundary".into();
        self.findings.push(finding);
    }

    fn report_missing_rationale(&mut self, expression: &ExprUnsafe) {
        if !self.allowed() || rationale(self.source, expression.span()) {
            return;
        }
        let mut finding = Finding::error(
            "unsafe-boundary",
            "unsafe block without SAFETY rationale",
            self.source.location(expression.span()),
        );
        finding.message =
            "an allowed unsafe block must have a nearby `SAFETY:` comment explaining its validity assumptions".into();
        finding.help =
            "place a concrete `// SAFETY: ...` rationale immediately before or at the start of the unsafe block".into();
        self.findings.push(finding);
    }
}

impl<'ast> Visit<'ast> for Syntax<'_> {
    fn visit_item_mod(&mut self, module: &'ast ItemMod) {
        let ffi = self.application && module.ident == "ffi";
        self.ffi_depth += usize::from(ffi);
        syn::visit::visit_item_mod(self, module);
        self.ffi_depth -= usize::from(ffi);
    }

    fn visit_expr_unsafe(&mut self, expression: &'ast ExprUnsafe) {
        self.report(expression.unsafe_token.span, "unsafe block".into(), "an unsafe block");
        self.report_missing_rationale(expression);
        syn::visit::visit_expr_unsafe(self, expression);
    }

    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        if let Some(token) = function.sig.unsafety {
            self.report(
                token.span,
                format!("unsafe function `{}`", function.sig.ident),
                "an unsafe function",
            );
        }
        syn::visit::visit_item_fn(self, function);
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        if let Some(token) = function.sig.unsafety {
            self.report(
                token.span,
                format!("unsafe method `{}`", function.sig.ident),
                "an unsafe method",
            );
        }
        syn::visit::visit_impl_item_fn(self, function);
    }

    fn visit_trait_item_fn(&mut self, function: &'ast TraitItemFn) {
        if let Some(token) = function.sig.unsafety {
            self.report(
                token.span,
                format!("unsafe trait method `{}`", function.sig.ident),
                "an unsafe trait method",
            );
        }
        syn::visit::visit_trait_item_fn(self, function);
    }

    fn visit_item_impl(&mut self, implementation: &'ast ItemImpl) {
        if let Some(token) = implementation.unsafety {
            self.report(token.span, "unsafe impl".into(), "an unsafe impl");
        }
        syn::visit::visit_item_impl(self, implementation);
    }

    fn visit_item_trait(&mut self, item: &'ast ItemTrait) {
        if let Some(token) = item.unsafety {
            self.report(token.span, format!("unsafe trait `{}`", item.ident), "an unsafe trait");
        }
        syn::visit::visit_item_trait(self, item);
    }
}

fn boundary(source: &Source) -> bool {
    let components = source
        .path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    components.windows(2).any(|components| components == ["src", "native"])
        || (source.package == "hl-engine" && components.windows(2).any(|components| components == ["src", "ffi"]))
        || components.windows(4).any(|components| {
            matches!(components[0], "app" | "apps")
                && components[2] == "src"
                && (components[3] == "ffi.rs" || components[3] == "ffi")
        })
}

fn application(source: &Source) -> bool {
    let components = source
        .path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    components
        .windows(2)
        .any(|components| components[0] == "src" && matches!(components[1], "app" | "apps"))
}

fn rationale(source: &Source, span: Span) -> bool {
    let lines = source.text.lines().collect::<Vec<_>>();
    let line = span.start().line.saturating_sub(1);
    let first = line.saturating_sub(3);
    let last = (line + 1).min(lines.len().saturating_sub(1));
    lines[first..=last].iter().any(|line| {
        ["// SAFETY:", "/* SAFETY:", "* SAFETY:"]
            .iter()
            .find_map(|marker| line.split_once(marker).map(|(_, rationale)| rationale))
            .is_some_and(|rationale| {
                rationale
                    .trim()
                    .trim_end_matches("*/")
                    .chars()
                    .any(char::is_alphanumeric)
            })
    })
}

#[cfg(test)]
#[path = "test.rs"]
mod tests;
