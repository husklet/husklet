use proc_macro2::{Span, TokenTree};
use syn::{
    Attribute, Expr, ExprUnsafe, ImplItemFn, ItemFn, ItemImpl, ItemMod, ItemTrait, Macro, Token, TraitItemFn,
    punctuated::Punctuated, spanned::Spanned, visit::Visit,
};

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
                boundary: boundary(source) || allows(&source.syntax.attrs),
                application: application(source),
                ffi_depth: 0,
                allow_depth: 0,
                macro_depth: 0,
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
    allow_depth: usize,
    macro_depth: usize,
    findings: Vec<Finding>,
}

impl Syntax<'_> {
    fn allowed(&self) -> bool {
        self.boundary || self.ffi_depth != 0 || self.allow_depth != 0
    }

    /// Opens the `#[allow(unsafe_code)]` scope of an item, returning the depth to release afterwards.
    fn enter(&mut self, attributes: &[Attribute]) -> usize {
        let allow = usize::from(allows(attributes));
        self.allow_depth += allow;
        allow
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

    /// A macro argument shares its caller's line, so the rationale window cannot be read there; the
    /// boundary rule still applies.
    fn report_missing_rationale(&mut self, expression: &ExprUnsafe) {
        if !self.allowed() || self.macro_depth != 0 || rationale(self.source, expression.span()) {
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
        let allow = self.enter(&module.attrs);
        syn::visit::visit_item_mod(self, module);
        self.allow_depth -= allow;
        self.ffi_depth -= usize::from(ffi);
    }

    /// `syn` does not walk macro token streams, so `assert_eq!(unsafe { .. }, 0)` would otherwise
    /// escape every check that bare `unsafe` beside it receives.
    fn visit_macro(&mut self, invocation: &'ast Macro) {
        let arguments = invocation.parse_body_with(Punctuated::<Expr, Token![,]>::parse_terminated);
        self.macro_depth += 1;
        for argument in arguments.iter().flatten() {
            self.visit_expr(argument);
        }
        self.macro_depth -= 1;
    }

    fn visit_expr_unsafe(&mut self, expression: &'ast ExprUnsafe) {
        let allow = self.enter(&expression.attrs);
        self.report(expression.unsafe_token.span, "unsafe block".into(), "an unsafe block");
        self.report_missing_rationale(expression);
        syn::visit::visit_expr_unsafe(self, expression);
        self.allow_depth -= allow;
    }

    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        let allow = self.enter(&function.attrs);
        if let Some(token) = function.sig.unsafety {
            self.report(
                token.span,
                format!("unsafe function `{}`", function.sig.ident),
                "an unsafe function",
            );
        }
        syn::visit::visit_item_fn(self, function);
        self.allow_depth -= allow;
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        let allow = self.enter(&function.attrs);
        if let Some(token) = function.sig.unsafety {
            self.report(
                token.span,
                format!("unsafe method `{}`", function.sig.ident),
                "an unsafe method",
            );
        }
        syn::visit::visit_impl_item_fn(self, function);
        self.allow_depth -= allow;
    }

    fn visit_trait_item_fn(&mut self, function: &'ast TraitItemFn) {
        let allow = self.enter(&function.attrs);
        if let Some(token) = function.sig.unsafety {
            self.report(
                token.span,
                format!("unsafe trait method `{}`", function.sig.ident),
                "an unsafe trait method",
            );
        }
        syn::visit::visit_trait_item_fn(self, function);
        self.allow_depth -= allow;
    }

    fn visit_item_impl(&mut self, implementation: &'ast ItemImpl) {
        let allow = self.enter(&implementation.attrs);
        if let Some(token) = implementation.unsafety {
            self.report(token.span, "unsafe impl".into(), "an unsafe impl");
        }
        syn::visit::visit_item_impl(self, implementation);
        self.allow_depth -= allow;
    }

    fn visit_item_trait(&mut self, item: &'ast ItemTrait) {
        let allow = self.enter(&item.attrs);
        if let Some(token) = item.unsafety {
            self.report(token.span, format!("unsafe trait `{}`", item.ident), "an unsafe trait");
        }
        syn::visit::visit_item_trait(self, item);
        self.allow_depth -= allow;
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

/// Recognises the compiler-enforced opt-out from the workspace `unsafe_code = "deny"` policy.
fn allows(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("allow")
            && attribute.meta.require_list().is_ok_and(|list| {
                list.tokens
                    .clone()
                    .into_iter()
                    .any(|token| matches!(token, TokenTree::Ident(ident) if ident == "unsafe_code"))
            })
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

/// Start of the comment block that runs unbroken up to `line`, so the window below is measured from
/// that block's last line rather than its first and a long rationale is not truncated away.
fn attached(lines: &[&str], line: usize) -> usize {
    let mut first = line;
    while first != 0 {
        let previous = lines[first - 1].trim_start();
        if !previous.starts_with("//") && !previous.starts_with('*') && !previous.starts_with("/*") {
            break;
        }
        first -= 1;
    }
    first
}

fn rationale(source: &Source, span: Span) -> bool {
    let lines = source.text.lines().collect::<Vec<_>>();
    let line = span.start().line.saturating_sub(1);
    let first = line.saturating_sub(3).min(attached(&lines, line));
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
