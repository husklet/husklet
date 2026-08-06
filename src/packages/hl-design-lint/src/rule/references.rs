use std::rc::Rc;

use syn::{ImplItemFn, ItemFn, spanned::Spanned, visit::Visit};

use crate::{Location, source::Source};

#[derive(Clone)]
pub struct Context {
    pub name: String,
    pub source: String,
}

#[derive(Clone)]
pub struct Reference {
    pub name: String,
    pub location: Location,
    pub context: Option<Rc<Context>>,
}

pub struct References<'a> {
    source: &'a Source,
    context: Option<Rc<Context>>,
    pub values: Vec<Reference>,
}

impl<'a> References<'a> {
    pub fn new(source: &'a Source) -> Self {
        Self {
            source,
            context: None,
            values: Vec::new(),
        }
    }

    fn context(&self, name: String, span: proc_macro2::Span) -> Rc<Context> {
        Rc::new(Context {
            name,
            source: self.source.excerpt(span),
        })
    }
}

impl<'ast> Visit<'ast> for References<'_> {
    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        let previous = self
            .context
            .replace(self.context(function.sig.ident.to_string(), function.span()));
        syn::visit::visit_item_fn(self, function);
        self.context = previous;
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        let previous = self
            .context
            .replace(self.context(function.sig.ident.to_string(), function.span()));
        syn::visit::visit_impl_item_fn(self, function);
        self.context = previous;
    }

    fn visit_expr_path(&mut self, expression: &'ast syn::ExprPath) {
        if expression.qself.is_none()
            && let Some(segment) = expression.path.segments.last() {
                self.values.push(Reference {
                    name: segment.ident.to_string(),
                    location: self.source.location(expression.span()),
                    context: self.context.clone(),
                });
            }
        syn::visit::visit_expr_path(self, expression);
    }
}
