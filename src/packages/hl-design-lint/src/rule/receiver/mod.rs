use syn::{FnArg, ImplItemFn, ItemImpl, ItemTrait, TraitItem, TraitItemFn, visit::Visit};

use crate::{
    Result,
    model::{Finding, Review, Severity},
    source::{Source, Workspace},
};

use super::{Rule, syntax::type_name};

/// Detects method names that unnecessarily repeat their receiver namespace.
pub struct Repetition;

impl Rule for Repetition {
    fn id(&self) -> &'static str {
        "receiver-name-repetition"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for source in workspace.production() {
            Visitor {
                rule: self.id(),
                source,
                findings: &mut findings,
            }
            .visit_file(&source.syntax);
        }
        Ok(findings)
    }
}

struct Visitor<'a> {
    rule: &'static str,
    source: &'a Source,
    findings: &'a mut Vec<Finding>,
}

impl Visitor<'_> {
    fn inspect(&mut self, namespace: &str, method: &syn::Ident, signature: &syn::Signature) {
        // Associated constructors and conversions have no receiver. Conversion-shaped receiver
        // methods are externally meaningful even when their destination repeats the source noun.
        if !signature.inputs.iter().any(is_receiver) {
            return;
        }

        let namespace_tokens = words(namespace);
        let method_tokens = words(&method.to_string());
        if namespace_tokens.is_empty()
            || method_tokens.len() <= namespace_tokens.len()
            || ambiguous(&namespace_tokens)
            || conversion(&method_tokens)
        {
            return;
        }

        let position = if method_tokens.starts_with(&namespace_tokens) {
            "prefix"
        } else if method_tokens.ends_with(&namespace_tokens) {
            "suffix"
        } else {
            return;
        };
        let method_name = method.to_string();
        let suggested = if position == "prefix" {
            method_tokens[namespace_tokens.len()..].join("_")
        } else {
            method_tokens[..method_tokens.len() - namespace_tokens.len()].join("_")
        };
        let mut finding = Finding::error(
            self.rule,
            format!("{namespace}::{method_name}"),
            self.source.location(method.span()),
        );
        finding.message = format!("method `{method_name}` repeats receiver namespace `{namespace}` as a {position}");
        finding.help = format!(
            "prefer `{namespace}::{suggested}` when the repeated words mean the receiver itself; retain them only when they name a distinct domain concept"
        );
        let mut review = Review::error();
        review.metadata = vec![
            ("Receiver namespace".into(), namespace.into()),
            ("Receiver tokens".into(), namespace_tokens.join(", ")),
            ("Method tokens".into(), method_tokens.join(", ")),
            ("Repeated position".into(), position.into()),
            ("Candidate name".into(), suggested),
        ];
        review.questions = vec![
            "Does the repeated token denote the receiver itself or a genuinely different domain concept?".into(),
            "Will the shorter method remain unambiguous at every call site and re-export boundary?".into(),
            "Have all callers and documentation been updated together?".into(),
        ];
        finding.review = Some(review);
        self.findings.push(finding);
    }
}

impl<'ast> Visit<'ast> for Visitor<'_> {
    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        // A foreign or local trait owns names in its implementation. The trait definition is
        // inspected independently when it is repository-owned.
        if item.trait_.is_some() {
            return;
        }
        let Some(namespace) = type_name(&item.self_ty) else {
            return;
        };
        for member in &item.items {
            if let syn::ImplItem::Fn(method) = member {
                self.inspect(&namespace, &method.sig.ident, &method.sig);
            }
        }
    }

    fn visit_item_trait(&mut self, item: &'ast ItemTrait) {
        let namespace = item.ident.to_string();
        for member in &item.items {
            if let TraitItem::Fn(method) = member {
                self.inspect(&namespace, &method.sig.ident, &method.sig);
            }
        }
    }

    fn visit_impl_item_fn(&mut self, _method: &'ast ImplItemFn) {}

    fn visit_trait_item_fn(&mut self, _method: &'ast TraitItemFn) {}
}

fn is_receiver(argument: &FnArg) -> bool {
    matches!(argument, FnArg::Receiver(_))
}

fn conversion(tokens: &[String]) -> bool {
    matches!(tokens.first().map(String::as_str), Some("as" | "from" | "into" | "to"))
        || matches!(
            tokens,
            [first, second, ..]
                if first == "try" && matches!(second.as_str(), "from" | "into")
        )
}

fn ambiguous(tokens: &[String]) -> bool {
    tokens
        .iter()
        .all(|token| token.len() < 3 || token.chars().all(|character| character.is_ascii_digit()))
}

fn words(value: &str) -> Vec<String> {
    let characters: Vec<char> = value.trim_start_matches("r#").chars().collect();
    let mut words = Vec::new();
    let mut start = 0;
    for index in 1..characters.len() {
        let previous = characters[index - 1];
        let current = characters[index];
        let next = characters.get(index + 1).copied();
        let boundary = !previous.is_ascii_alphanumeric()
            || !current.is_ascii_alphanumeric()
            || (previous.is_ascii_lowercase() && current.is_ascii_uppercase())
            || (previous.is_ascii_uppercase()
                && current.is_ascii_uppercase()
                && next.is_some_and(|next| next.is_ascii_lowercase()))
            || (previous.is_ascii_alphabetic() && current.is_ascii_digit())
            || (previous.is_ascii_digit() && current.is_ascii_alphabetic());
        if boundary {
            push_word(&characters[start..index], &mut words);
            start = if current.is_ascii_alphanumeric() {
                index
            } else {
                index + 1
            };
        }
    }
    push_word(&characters[start..], &mut words);
    words
}

fn push_word(characters: &[char], words: &mut Vec<String>) {
    let word: String = characters
        .iter()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(|character| character.to_lowercase())
        .collect();
    if !word.is_empty() {
        words.push(word);
    }
}
