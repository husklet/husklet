use proc_macro2::Span;
use syn::{spanned::Spanned, visit::Visit, Expr, ExprCall, ExprMacro, ImplItemFn, ItemFn};

use crate::{
    model::{Finding, Severity},
    rule::Rule,
    source::Workspace,
    Result,
};

/// Reports ambient environment-variable access.
pub struct EnvironmentAccess;

impl Rule for EnvironmentAccess {
    fn id(&self) -> &'static str {
        "environment-variable-access"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for source in workspace.sources() {
            let mut visitor = Accesses {
                context: None,
                spans: Vec::new(),
            };
            visitor.visit_file(&source.syntax);
            for (span, context) in visitor.spans {
                let location = source.location(span);
                let access = location.source.clone();
                let violation = source.domain == "packages";
                let mut finding = Finding::warning(self.id(), access.clone(), location);
                finding.message = format!(
                    "environment access `{access}` ({})",
                    if violation {
                        "package violation"
                    } else {
                        "review required"
                    }
                );
                finding.help = "accept a typed value from application CLI/configuration instead of reading ambient process state".to_owned();
                finding.related.push(crate::model::Related {
                    label: format!("enclosing function `{}`", context.0),
                    location: source.location(context.1),
                });
                findings.push(finding);
            }
        }
        Ok(findings)
    }
}

struct Accesses {
    context: Option<(String, Span)>,
    spans: Vec<(Span, (String, Span))>,
}

impl Accesses {
    fn record(&mut self, span: Span) {
        let context = self
            .context
            .clone()
            .unwrap_or_else(|| ("module scope".to_owned(), span));
        self.spans.push((span, context));
    }

    fn context(&mut self, name: String, span: Span, visit: impl FnOnce(&mut Self)) {
        let previous = self.context.replace((name, span));
        visit(self);
        self.context = previous;
    }
}

impl<'ast> Visit<'ast> for Accesses {
    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        self.context(function.sig.ident.to_string(), function.span(), |visitor| {
            syn::visit::visit_item_fn(visitor, function)
        });
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        self.context(function.sig.ident.to_string(), function.span(), |visitor| {
            syn::visit::visit_impl_item_fn(visitor, function)
        });
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if environment_call(call) {
            self.record(call.span());
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_macro(&mut self, expression: &'ast ExprMacro) {
        let name = expression
            .mac
            .path
            .segments
            .last()
            .map(|segment| &segment.ident);
        if name.is_some_and(|name| name == "env" || name == "option_env") {
            self.record(expression.span());
        }
        syn::visit::visit_expr_macro(self, expression);
    }
}

fn environment_call(call: &ExprCall) -> bool {
    let Expr::Path(function) = call.func.as_ref() else {
        return false;
    };
    let segments = function.path.segments.iter().collect::<Vec<_>>();
    let Some(method) = segments.last() else {
        return false;
    };
    segments
        .iter()
        .rev()
        .nth(1)
        .is_some_and(|segment| segment.ident == "env")
        && matches!(
            method.ident.to_string().as_str(),
            "var" | "var_os" | "vars" | "vars_os" | "set_var" | "remove_var"
        )
}
