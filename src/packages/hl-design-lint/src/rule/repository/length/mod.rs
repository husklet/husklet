use proc_macro2::Span;
use syn::{spanned::Spanned, visit::Visit};

use crate::{
    Result,
    model::{Finding, Review, Severity},
    rule::Rule,
    rule::support::syntax::{item_attributes, test_only},
    source::Workspace,
};

/// Rejects Rust files whose size obscures cohesive ownership boundaries.
pub struct FileLength;

#[cfg(test)]
#[path = "test.rs"]
mod tests;

impl Rule for FileLength {
    fn id(&self) -> &'static str {
        "file-length"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for source in workspace.production() {
            let physical = source.text.lines().count();
            let mut excluded = vec![false; physical.saturating_add(1)];
            ExcludedLines {
                physical,
                lines: &mut excluded,
            }
            .visit_file(&source.syntax);
            let lines = (1..=physical).filter(|line| !excluded[*line]).count();
            if lines <= 500 {
                continue;
            }
            let span = source
                .syntax
                .items
                .first()
                .map_or_else(Span::call_site, syn::Item::span);
            let subject = source
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("source file")
                .to_owned();
            let mut finding = Finding::error(self.id(), subject, source.location(span));
            finding.message = format!("Rust source contains {lines} lines; the maximum is 500");
            finding.help = "split by cohesive entity, component, screen region, adapter, or service; do not use include! or arbitrary numbered fragments".to_owned();
            let mut review = Review::error();
            review.metadata.push(("lines".to_owned(), lines.to_string()));
            review.metadata.push(("limit".to_owned(), "500".to_owned()));
            review.questions = vec![
                "Which independent responsibilities are mixed in this file?".to_owned(),
                "Does each extracted module have a precise domain name and dependency direction?".to_owned(),
                "Can the split be tested without relying on source-text assertions?".to_owned(),
            ];
            finding.review = Some(review);
            findings.push(finding);
        }
        Ok(findings)
    }
}

struct ExcludedLines<'a> {
    physical: usize,
    lines: &'a mut [bool],
}

impl ExcludedLines<'_> {
    fn mark(&mut self, item: &syn::Item) {
        let span = item.span();
        let start = item_attributes(item)
            .and_then(|attributes| attributes.first())
            .map_or_else(|| span.start().line, |attribute| attribute.span().start().line)
            .max(1);
        let end = span.end().line.min(self.physical);
        for line in start..=end {
            self.lines[line] = true;
        }
    }
}

impl<'ast> Visit<'ast> for ExcludedLines<'_> {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if test_only(item) || declaration(item) {
            self.mark(item);
            return;
        }
        syn::visit::visit_item(self, item);
    }
}

/// A file whose length tracks an external enumeration is sized by that enumeration, not by mixed
/// responsibility, so declaration items do not count toward the limit.
fn declaration(item: &syn::Item) -> bool {
    use syn::Item;

    matches!(item, Item::Enum(_) | Item::Struct(_) | Item::Const(_) | Item::Macro(_))
}
