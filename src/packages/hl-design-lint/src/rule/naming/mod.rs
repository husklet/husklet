use english_pos_tagger::Tagger;
use proc_macro2::Span;
use syn::{Attribute, ItemMod, ItemStruct, Meta, spanned::Spanned, visit::Visit};
use wordnet_lemmatizer::{Lemmatizer, Pos};

use crate::{
    Result,
    model::{Finding, Review, Severity},
    rule::Rule,
    source::{Source, Workspace, requires_test, snake_case},
};

/// Validates noun struct names using English POS tagging.
pub struct StructNaming;

impl Rule for StructNaming {
    fn id(&self) -> &'static str {
        "struct-noun-naming"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        let language = English::new();
        for source in workspace.production() {
            let mut names = Names {
                rule: self.id(),
                source,
                test_scope: false,
                findings: &mut findings,
                language: &language,
            };
            names.visit_file(&source.syntax);
        }
        Ok(findings)
    }
}

struct Names<'a> {
    rule: &'static str,
    source: &'a Source,
    test_scope: bool,
    findings: &'a mut Vec<Finding>,
    language: &'a dyn Language,
}

impl Names<'_> {
    fn push(&mut self, name: &syn::Ident, span: Span, kind: &str, reason: &str) {
        let mut finding = Finding::error(self.rule, name.to_string(), self.source.location(span));
        finding.message = format!("`{name}` violates the noun/struct rule: {reason}");
        finding.help =
            "choose a precise noun for the struct, or use #[hl_design::naming(reason = \"...\")] after human review"
                .into();
        let mut review = Review::error();
        review.metadata.push(("Kind".into(), kind.into()));
        review
            .questions
            .push("What domain noun states this value's role?".into());
        finding.review = Some(review);
        self.findings.push(finding);
    }

    fn type_name(&mut self, name: &syn::Ident, attributes: &[Attribute], span: Span) {
        if !self.test_scope && !exception(attributes) && !has_noun(self.language, &name.to_string()) {
            self.push(
                name,
                span,
                "struct",
                "type name is an adjective or past participle; name the value it represents",
            );
        }
    }
}

impl<'ast> Visit<'ast> for Names<'_> {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        let previous = self.test_scope;
        self.test_scope |= requires_test(&item.attrs);
        syn::visit::visit_item_mod(self, item);
        self.test_scope = previous;
    }

    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        self.type_name(&item.ident, &item.attrs, item.span());
        syn::visit::visit_item_struct(self, item);
    }
}

fn exception(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        if attribute
            .path()
            .segments
            .last()
            .is_none_or(|segment| segment.ident != "naming")
        {
            return false;
        }
        let Meta::List(list) = &attribute.meta else {
            return false;
        };
        let Ok(value) = list.parse_args::<syn::MetaNameValue>() else {
            return false;
        };
        if !value.path.is_ident("reason") {
            return false;
        }
        let syn::Expr::Lit(literal) = value.value else {
            return false;
        };
        let syn::Lit::Str(reason) = literal.lit else {
            return false;
        };
        !reason.value().trim().is_empty()
    })
}

trait Language {
    fn noun(&self, word: &str) -> bool;
}

struct English {
    tagger: Tagger,
    wordnet: Lemmatizer,
}

impl English {
    fn new() -> Self {
        Self {
            tagger: Tagger::new(),
            wordnet: Lemmatizer::embedded(),
        }
    }
}

impl Language for English {
    fn noun(&self, word: &str) -> bool {
        self.wordnet.morphy(word, Pos::Noun).is_some()
            || self
                .tagger
                .tag_raw_tokens(&[word])
                .first()
                .and_then(|token| token.pos.as_deref())
                .is_some_and(|part| part.starts_with("NN"))
    }
}

fn has_noun(language: &dyn Language, name: &str) -> bool {
    identifier_words(name).iter().any(|word| language.noun(word))
}

fn identifier_words(name: &str) -> Vec<String> {
    snake_case(name)
        .split('_')
        .map(|word| {
            word.chars()
                .filter(char::is_ascii_alphabetic)
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{English, Language, has_noun, identifier_words};

    #[test]
    fn classifies_domain_nouns() {
        let english = English::new();
        assert!(english.noun("workspace"));
        assert!(english.noun("settings"));
        assert!(english.noun("archive"));
        assert!(english.noun("call"));
        assert!(english.noun("capture"));
        assert!(english.noun("mount"));
        assert!(english.noun("register"));
        assert!(!english.noun("selected"));
    }

    #[test]
    fn accepts_versioned_nouns() {
        let english = English::new();
        assert_eq!(identifier_words("VkImageCopy2"), ["vk", "image", "copy"]);
        assert!(has_noun(&english, "VkImageCopy2"));
        assert!(!has_noun(&english, "Selected"));
    }
}
