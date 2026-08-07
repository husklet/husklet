use std::rc::Rc;

use syn::{
    Attribute, Expr, ImplItemFn, ItemFn, ItemImpl, ItemMod, Lit, Meta, Token, parse::Parser, punctuated::Punctuated,
    spanned::Spanned, visit::Visit,
};

use crate::{
    Location,
    source::{Source, requires_test},
};

#[derive(Clone)]
pub struct Context {
    pub name: String,
    pub source: String,
    /// The enclosing item is an inherent or trait method, so it is not a free function.
    pub associated: bool,
}

#[derive(Clone)]
pub struct Reference {
    pub name: String,
    pub location: Location,
    pub context: Option<Rc<Context>>,
    /// The reference lives in an attribute body rather than ordinary code.
    pub attribute: bool,
    /// Module named immediately before the reference, when it was written with one.
    pub module: Option<String>,
}

pub struct References<'a> {
    source: &'a Source,
    context: Option<Rc<Context>>,
    attribute: bool,
    test_scope: bool,
    scopes: Vec<Vec<String>>,
    pub values: Vec<Reference>,
}

impl<'a> References<'a> {
    pub fn new(source: &'a Source) -> Self {
        Self {
            source,
            context: None,
            attribute: false,
            test_scope: false,
            scopes: Vec::new(),
            values: Vec::new(),
        }
    }

    /// Runs `visit` with `bindings` in scope, so a name a local binding owns is read as the local.
    fn bound(&mut self, bindings: Vec<String>, visit: impl FnOnce(&mut Self)) {
        self.scopes.push(bindings);
        visit(self);
        self.scopes.pop();
    }

    fn shadowed(&self, name: &str) -> bool {
        self.scopes.iter().flatten().any(|binding| binding == name)
    }

    /// Runs `visit` with test scope widened by `attributes`, so uses under a test gate are not
    /// counted as production uses.
    fn scoped(&mut self, attributes: &[Attribute], visit: impl FnOnce(&mut Self)) {
        let previous = self.test_scope;
        self.test_scope |= test_gated(attributes);
        visit(self);
        self.test_scope = previous;
    }

    fn context(&self, name: String, span: proc_macro2::Span, associated: bool) -> Rc<Context> {
        Rc::new(Context {
            name,
            source: self.source.excerpt(span),
            associated,
        })
    }

    fn push(&mut self, name: String, span: proc_macro2::Span, module: Option<String>) {
        if self.test_scope || (module.is_none() && self.shadowed(&name)) {
            return;
        }
        self.values.push(Reference {
            name,
            location: self.source.location(span),
            context: self.context.clone(),
            attribute: self.attribute,
            module,
        });
    }

    /// Reads calls out of a macro body: an ident directly followed by a parenthesis group, unless
    /// a `.` precedes it, which makes it a method rather than a free function.
    fn tokens(&mut self, stream: proc_macro2::TokenStream) {
        let mut pending: Option<proc_macro2::Ident> = None;
        let mut dotted = false;
        for token in stream {
            match token {
                proc_macro2::TokenTree::Ident(ident) => {
                    pending = (!dotted).then(|| ident.clone());
                    dotted = false;
                }
                proc_macro2::TokenTree::Group(group) => {
                    if let Some(ident) = pending.take()
                        && group.delimiter() == proc_macro2::Delimiter::Parenthesis
                    {
                        self.push(ident.to_string(), ident.span(), None);
                    }
                    self.tokens(group.stream());
                    dotted = false;
                }
                proc_macro2::TokenTree::Punct(punct) => {
                    pending = None;
                    dotted = punct.as_char() == '.';
                }
                proc_macro2::TokenTree::Literal(_) => {
                    pending = None;
                    dotted = false;
                }
            }
        }
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
                self.push(segment.ident.to_string(), text.span(), qualifier(&path));
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
            .replace(self.context(function.sig.ident.to_string(), function.span(), false));
        let parameters = signature(&function.sig);
        self.scoped(&function.attrs, |visitor| {
            visitor.bound(parameters, |visitor| {
                syn::visit::visit_item_fn(visitor, function);
            });
        });
        self.context = previous;
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        let previous = self
            .context
            .replace(self.context(function.sig.ident.to_string(), function.span(), true));
        let parameters = signature(&function.sig);
        self.scoped(&function.attrs, |visitor| {
            visitor.bound(parameters, |visitor| {
                syn::visit::visit_impl_item_fn(visitor, function);
            });
        });
        self.context = previous;
    }

    fn visit_block(&mut self, block: &'ast syn::Block) {
        self.bound(Vec::new(), |visitor| {
            syn::visit::visit_block(visitor, block);
        });
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        syn::visit::visit_local(self, local);
        let mut names = Vec::new();
        bindings(&local.pat, &mut names);
        if let Some(scope) = self.scopes.last_mut() {
            scope.extend(names);
        }
    }

    fn visit_expr_closure(&mut self, closure: &'ast syn::ExprClosure) {
        let mut names = Vec::new();
        for input in &closure.inputs {
            bindings(input, &mut names);
        }
        self.bound(names, |visitor| {
            syn::visit::visit_expr_closure(visitor, closure);
        });
    }

    fn visit_expr_for_loop(&mut self, loop_expression: &'ast syn::ExprForLoop) {
        let mut names = Vec::new();
        bindings(&loop_expression.pat, &mut names);
        self.bound(names, |visitor| {
            syn::visit::visit_expr_for_loop(visitor, loop_expression);
        });
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        let mut names = Vec::new();
        bindings(&arm.pat, &mut names);
        self.bound(names, |visitor| {
            syn::visit::visit_arm(visitor, arm);
        });
    }

    fn visit_expr_let(&mut self, expression: &'ast syn::ExprLet) {
        syn::visit::visit_expr_let(self, expression);
        let mut names = Vec::new();
        bindings(&expression.pat, &mut names);
        if let Some(scope) = self.scopes.last_mut() {
            scope.extend(names);
        }
    }

    fn visit_item_mod(&mut self, module: &'ast ItemMod) {
        self.scoped(&module.attrs, |visitor| {
            syn::visit::visit_item_mod(visitor, module);
        });
    }

    fn visit_item_impl(&mut self, block: &'ast ItemImpl) {
        self.scoped(&block.attrs, |visitor| {
            syn::visit::visit_item_impl(visitor, block);
        });
    }

    fn visit_item_const(&mut self, item: &'ast syn::ItemConst) {
        self.scoped(&item.attrs, |visitor| {
            syn::visit::visit_item_const(visitor, item);
        });
    }

    fn visit_item_static(&mut self, item: &'ast syn::ItemStatic) {
        self.scoped(&item.attrs, |visitor| {
            syn::visit::visit_item_static(visitor, item);
        });
    }

    fn visit_expr_path(&mut self, expression: &'ast syn::ExprPath) {
        if expression.qself.is_none()
            && let Some(segment) = expression.path.segments.last()
        {
            let name = segment.ident.to_string();
            self.push(name, expression.span(), qualifier(&expression.path));
        }
        syn::visit::visit_expr_path(self, expression);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if mac.path.is_ident("macro_rules") {
            return;
        }
        self.tokens(mac.tokens.clone());
    }

    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        let previous = std::mem::replace(&mut self.attribute, true);
        self.meta(&attribute.meta);
        self.attribute = previous;
    }
}

/// The module a path names before its final segment, if it names one. `Self::f` and `Type::f`
/// report their type, which no module is spelled as, so they never match a free function.
fn qualifier(path: &syn::Path) -> Option<String> {
    let segment = path.segments.iter().rev().nth(1)?;
    let name = segment.ident.to_string();
    (!matches!(name.as_str(), "crate" | "self" | "super")).then_some(name)
}

/// The module an item defined in `path` under `nesting` inline modules belongs to.
pub fn module(path: &std::path::Path, nesting: &[String]) -> String {
    if let Some(name) = nesting.last() {
        return name.clone();
    }
    let stem = path.file_stem().map(|stem| stem.to_string_lossy().into_owned());
    match stem.as_deref() {
        Some("mod" | "lib" | "main") | None => path
            .parent()
            .and_then(|parent| parent.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        Some(_) => stem.unwrap_or_default(),
    }
}

fn signature(sig: &syn::Signature) -> Vec<String> {
    let mut names = Vec::new();
    for input in &sig.inputs {
        match input {
            syn::FnArg::Receiver(_) => names.push("self".to_owned()),
            syn::FnArg::Typed(argument) => bindings(&argument.pat, &mut names),
        }
    }
    names
}

fn bindings(pattern: &syn::Pat, names: &mut Vec<String>) {
    struct Names<'a>(&'a mut Vec<String>);
    impl<'ast> Visit<'ast> for Names<'_> {
        fn visit_pat_ident(&mut self, pattern: &'ast syn::PatIdent) {
            self.0.push(pattern.ident.to_string());
            syn::visit::visit_pat_ident(self, pattern);
        }
    }
    Names(names).visit_pat(pattern);
}

fn test_gated(attributes: &[Attribute]) -> bool {
    requires_test(attributes)
        || attributes.iter().any(|attribute| {
            attribute
                .path()
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "test" || segment.ident == "bench")
        })
}
