use std::rc::Rc;

use syn::{
    Attribute, Expr, ImplItemFn, ItemFn, Lit, Meta, Token, parse::Parser, punctuated::Punctuated, spanned::Spanned,
    visit::Visit,
};

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
    /// The reference lives in an attribute body rather than ordinary code.
    pub attribute: bool,
}

pub struct References<'a> {
    source: &'a Source,
    context: Option<Rc<Context>>,
    attribute: bool,
    pub values: Vec<Reference>,
}

impl<'a> References<'a> {
    pub fn new(source: &'a Source) -> Self {
        Self {
            source,
            context: None,
            attribute: false,
            values: Vec::new(),
        }
    }

    fn context(&self, name: String, span: proc_macro2::Span) -> Rc<Context> {
        Rc::new(Context {
            name,
            source: self.source.excerpt(span),
        })
    }

    fn push(&mut self, name: String, span: proc_macro2::Span) {
        self.values.push(Reference {
            name,
            location: self.source.location(span),
            context: self.context.clone(),
            attribute: self.attribute,
        });
    }

    /// Walks `key = value` pairs of an attribute; a bare path is a macro flag, not a use.
    fn meta(&mut self, meta: &Meta) {
        match meta {
            Meta::Path(_) => {}
            Meta::List(list) => {
                if let Ok(nested) = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(list.tokens.clone()) {
                    for meta in &nested {
                        self.meta(meta);
                    }
                }
            }
            Meta::NameValue(pair) => self.value(pair),
        }
    }

    /// A string value is only a path under serde's path-valued keys; elsewhere it is prose.
    fn value(&mut self, pair: &syn::MetaNameValue) {
        if let Expr::Lit(literal) = &pair.value
            && let Lit::Str(text) = &literal.lit
        {
            if pair.path.segments.last().is_some_and(|segment| {
                matches!(
                    segment.ident.to_string().as_str(),
                    "with" | "serialize_with" | "deserialize_with" | "default" | "skip_serializing_if" | "getter"
                )
            }) && let Ok(path) = text.parse::<syn::Path>()
                && let Some(segment) = path.segments.last()
            {
                self.push(segment.ident.to_string(), text.span());
            }
            return;
        }
        syn::visit::visit_expr(self, &pair.value);
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
            && let Some(segment) = expression.path.segments.last()
        {
            let name = segment.ident.to_string();
            self.push(name, expression.span());
        }
        syn::visit::visit_expr_path(self, expression);
    }

    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        let previous = std::mem::replace(&mut self.attribute, true);
        self.meta(&attribute.meta);
        self.attribute = previous;
    }
}
