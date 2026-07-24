use syn::{visit::Visit, Expr, Lit, Pat, Type};

pub(super) fn excluded_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "text",
        "message",
        "description",
        "detail",
        "error",
        "name",
        "title",
        "label",
        "path",
        "url",
        "uri",
        "id",
        "identifier",
        "reference",
        "command",
        "query",
        "header",
        "body",
        "content",
        "log",
        "output",
        "input",
        "format",
        "mime",
        "media",
        "user",
        "token",
        "key",
        "value",
        "raw",
    ]
    .iter()
    .any(|word| name == *word || name.ends_with(&format!("_{word}")))
}

pub(super) fn state_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "state",
        "status",
        "phase",
        "mode",
        "kind",
        "stage",
        "condition",
        "lifecycle",
        "action",
    ]
    .iter()
    .any(|word| name == *word || name.ends_with(&format!("_{word}")))
}

pub(super) fn peel(expression: &Expr) -> &Expr {
    match expression {
        Expr::Paren(inner) => peel(&inner.expr),
        Expr::Reference(inner) => peel(&inner.expr),
        Expr::MethodCall(call)
            if matches!(
                call.method.to_string().as_str(),
                "as_str" | "as_ref" | "borrow"
            ) && call.args.is_empty() =>
        {
            peel(&call.receiver)
        }
        _ => expression,
    }
}

pub(super) fn string_literal(expression: &Expr) -> Option<(String, proc_macro2::Span)> {
    match peel(expression) {
        Expr::Lit(literal) => match &literal.lit {
            Lit::Str(value) => Some((value.value(), value.span())),
            _ => None,
        },
        Expr::MethodCall(call)
            if matches!(
                call.method.to_string().as_str(),
                "into" | "to_owned" | "to_string"
            ) && call.args.is_empty() =>
        {
            string_literal(&call.receiver)
        }
        Expr::Call(call) if call.args.len() == 1 => string_literal(call.args.first()?),
        _ => None,
    }
}

pub(super) fn pattern_literals(pattern: &Pat) -> Vec<(String, proc_macro2::Span)> {
    match pattern {
        Pat::Lit(literal) => match &literal.lit {
            Lit::Str(value) => vec![(value.value(), value.span())],
            _ => Vec::new(),
        },
        Pat::Or(or) => or.cases.iter().flat_map(pattern_literals).collect(),
        Pat::Paren(paren) => pattern_literals(&paren.pat),
        _ => Vec::new(),
    }
}

pub(super) fn preserves_unknown(pattern: &Pat, body: &Expr, concept: &str) -> bool {
    let name = match pattern {
        Pat::Ident(binding) => binding.ident.to_string(),
        Pat::Wild(_) => concept.to_owned(),
        Pat::Paren(paren) => return preserves_unknown(&paren.pat, body, concept),
        Pat::Reference(reference) => return preserves_unknown(&reference.pat, body, concept),
        _ => return false,
    };
    references_name(body, &name) && (unknown_marker(body) || direct_reference(body, &name))
}

fn references_name(expression: &Expr, name: &str) -> bool {
    let mut references = References { name, found: false };
    references.visit_expr(expression);
    references.found
}

fn unknown_marker(expression: &Expr) -> bool {
    let mut markers = UnknownMarker(false);
    markers.visit_expr(expression);
    markers.0
}

fn direct_reference(expression: &Expr, name: &str) -> bool {
    match expression {
        Expr::Path(path) => path.path.is_ident(name),
        Expr::Field(field) => {
            matches!(&field.member, syn::Member::Named(member) if member == name)
        }
        Expr::Paren(paren) => direct_reference(&paren.expr, name),
        Expr::Reference(reference) => direct_reference(&reference.expr, name),
        Expr::MethodCall(call)
            if matches!(
                call.method.to_string().as_str(),
                "clone" | "into" | "to_owned" | "to_string"
            ) && call.args.is_empty() =>
        {
            direct_reference(&call.receiver, name)
        }
        _ => false,
    }
}

struct References<'a> {
    name: &'a str,
    found: bool,
}

impl<'ast> Visit<'ast> for References<'_> {
    fn visit_expr_path(&mut self, expression: &'ast syn::ExprPath) {
        self.found |= expression
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == self.name);
        if !self.found {
            syn::visit::visit_expr_path(self, expression);
        }
    }

    fn visit_expr_field(&mut self, expression: &'ast syn::ExprField) {
        self.found |= matches!(
            &expression.member,
            syn::Member::Named(member) if member == self.name
        );
        if !self.found {
            syn::visit::visit_expr_field(self, expression);
        }
    }
}

struct UnknownMarker(bool);

impl<'ast> Visit<'ast> for UnknownMarker {
    fn visit_expr_path(&mut self, expression: &'ast syn::ExprPath) {
        self.0 |= expression.path.segments.iter().any(|segment| {
            matches!(
                segment.ident.to_string().as_str(),
                "Unknown" | "Unrecognized" | "Other" | "Raw" | "Custom"
            )
        });
        if !self.0 {
            syn::visit::visit_expr_path(self, expression);
        }
    }
}

pub(super) fn is_self(expression: &Expr) -> bool {
    matches!(
        peel(expression),
        Expr::Path(path)
            if path.qself.is_none() && path.path.is_ident("self")
    )
}

pub(super) fn type_name(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else {
        return None;
    };
    Some(path.path.segments.last()?.ident.to_string())
}

pub(super) fn string_type(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Path(path)
            if path.path.segments.last().is_some_and(|segment| {
                matches!(segment.ident.to_string().as_str(), "String" | "str")
            })
    ) || matches!(
        ty,
        Type::Reference(reference) if string_type(&reference.elem)
    )
}
