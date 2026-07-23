use std::collections::{BTreeSet, HashMap};

use syn::{
    spanned::Spanned, visit::Visit, Expr, ExprCall, ExprMacro, FnArg, ItemFn, ItemMod,
    PathArguments, Type,
};

use crate::{
    model::{Finding, Related, Review, ReviewState, Severity},
    rule::{usage::References, Rule},
    source::{requires_test, Workspace},
    Result,
};

/// Requires one- and two-argument free functions to be refactored or classified.
pub struct FreeFunction;

impl Rule for FreeFunction {
    fn id(&self) -> &'static str {
        "unclassified-free-function"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut candidates = Vec::new();
        let mut definitions = HashMap::<String, usize>::new();
        let mut references = HashMap::<String, Vec<crate::rule::usage::Reference>>::new();
        for source in workspace.production() {
            let mut functions = Functions {
                test_scope: false,
                values: Vec::new(),
            };
            functions.visit_file(&source.syntax);
            for value in functions.values {
                *definitions.entry(value.name.clone()).or_default() += 1;
                candidates.push((source, value));
            }
            let mut uses = References::new(source);
            uses.visit_file(&source.syntax);
            for reference in uses.values {
                references
                    .entry(reference.name.clone())
                    .or_default()
                    .push(reference);
            }
        }
        Ok(candidates
            .into_iter()
            .map(|(source, candidate)| {
                let ambiguous = definitions.get(&candidate.name).copied().unwrap_or(0) != 1;
                let usages = references
                    .get(&candidate.name)
                    .into_iter()
                    .flatten()
                    .filter(|usage| !ambiguous || usage.location.path == source.path);
                candidate.finding(self.id(), source, usages, ambiguous)
            })
            .collect())
    }
}

struct Candidate {
    name: String,
    arguments: usize,
    span: proc_macro2::Span,
    dependencies: Vec<String>,
    classification: Option<Classification>,
}

impl Candidate {
    fn finding<'a>(
        self,
        rule: &'static str,
        source: &crate::source::Source,
        usages: impl Iterator<Item = &'a crate::rule::usage::Reference>,
        ambiguous: bool,
    ) -> Finding {
        let mut finding = Finding::error(rule, &self.name, source.location(self.span));
        finding.message = format!(
            "unclassified free function `{}` has {} argument{}",
            self.name,
            self.arguments,
            if self.arguments == 1 { "" } else { "s" }
        );
        finding.help =
            "refactor it or add a temporary #[hl_design::classify(...)] classification".into();
        finding.related = usages
            .map(|usage| Related {
                label: usage.context.as_ref().map_or_else(
                    || "usage at module scope".into(),
                    |context| format!("usage in `{}`\n{}", context.name, context.source),
                ),
                location: usage.location.clone(),
            })
            .collect();
        let (state, classification) = self.classification.map_or_else(
            || (ReviewState::Error, "unclassified".to_owned()),
            |value| {
                let value = value.resolve(&source.package);
                let text = format!("{}({})", value.scope, value.kind);
                (ReviewState::Check(text.clone()), text)
            },
        );
        let review = Review {
            state,
            metadata: vec![
                ("Arguments".into(), self.arguments.to_string()),
                ("Classification".into(), classification),
                (
                    "Usage resolution".into(),
                    if ambiguous {
                        "ambiguous name; same-file references only"
                    } else {
                        "unique name in scanned tree"
                    }
                    .into(),
                ),
            ],
            dependencies: self.dependencies,
            questions: vec![
                "Does one argument already have a meaningful receiver type?".into(),
                "Do related functions share this value and its invariants?".into(),
                "Would a wrapper collect cohesive behavior, or only hide one helper?".into(),
                "Is this a complete low-level algorithm that should remain free?".into(),
            ],
        };
        finding.review = Some(review);
        finding
    }
}

struct Functions {
    test_scope: bool,
    values: Vec<Candidate>,
}

impl<'ast> Visit<'ast> for Functions {
    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        if !self.test_scope && !requires_test(&function.attrs) && candidate(function) {
            let mut dependencies = Dependencies::default();
            dependencies.visit_item_fn(function);
            let span = function
                .attrs
                .first()
                .and_then(|attribute| attribute.span().join(function.span()))
                .unwrap_or_else(|| function.span());
            self.values.push(Candidate {
                name: function.sig.ident.to_string(),
                arguments: function.sig.inputs.len(),
                span,
                dependencies: dependencies.names.into_iter().collect(),
                classification: classification(function),
            });
        }
        syn::visit::visit_item_fn(self, function);
    }

    fn visit_item_mod(&mut self, module: &'ast ItemMod) {
        let previous = self.test_scope;
        self.test_scope |= requires_test(&module.attrs);
        syn::visit::visit_item_mod(self, module);
        self.test_scope = previous;
    }
}

fn candidate(function: &ItemFn) -> bool {
    function.sig.abi.is_none()
        && matches!(function.sig.inputs.len(), 1 | 2)
        && !framework_adapter(function)
        && !function.attrs.iter().any(|attribute| {
            let path = attribute.path();
            path.is_ident("proc_macro")
                || path.is_ident("proc_macro_attribute")
                || path.is_ident("proc_macro_derive")
        })
}

fn framework_adapter(function: &ItemFn) -> bool {
    if !function.attrs.iter().any(|attribute| {
        attribute
            .path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "adapter")
    }) {
        return false;
    }
    function.sig.inputs.iter().any(|argument| {
        let FnArg::Typed(argument) = argument else {
            return false;
        };
        let Type::Path(ty) = argument.ty.as_ref() else {
            return false;
        };
        ty.path.segments.last().is_some_and(|segment| {
            let name = segment.ident.to_string();
            let generic = matches!(segment.arguments, PathArguments::AngleBracketed(_));
            (generic
                && matches!(
                    name.as_str(),
                    "State" | "Path" | "Query" | "Json" | "Form" | "Extension" | "ConnectInfo"
                ))
                || matches!(
                    name.as_str(),
                    "OriginalUri" | "RawQuery" | "WebSocketUpgrade" | "Multipart"
                )
        })
    })
}

#[derive(Clone)]
struct Classification {
    scope: String,
    kind: String,
}

impl Classification {
    fn resolve(mut self, package: &str) -> Self {
        if self.scope == "pkg" {
            self.kind = package.to_owned();
        }
        self
    }
}

fn classification(function: &ItemFn) -> Option<Classification> {
    let attribute = function.attrs.iter().find(|attribute| {
        attribute
            .path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "classify")
    })?;
    let syn::Meta::List(list) = &attribute.meta else {
        return None;
    };
    let text = list.tokens.to_string();
    if text.trim() == "pkg" {
        return Some(Classification {
            scope: "pkg".into(),
            kind: String::new(),
        });
    }
    let (scope, kind) = text.split_once('=')?;
    let scope = scope.trim().trim_start_matches("r#");
    let kind = kind.trim().trim_matches('"');
    (matches!(scope, "root" | "domain" | "pkg" | "struct") && !kind.is_empty()).then(|| {
        Classification {
            scope: scope.into(),
            kind: kind.into(),
        }
    })
}

#[derive(Default)]
struct Dependencies {
    names: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for Dependencies {
    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Expr::Path(function) = call.func.as_ref() {
            self.names.insert(path_name(&function.path));
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.names.insert(format!(".{}", call.method));
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_macro(&mut self, expression: &'ast ExprMacro) {
        self.names
            .insert(format!("{}!", path_name(&expression.mac.path)));
        syn::visit::visit_expr_macro(self, expression);
    }
}

fn path_name(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}
