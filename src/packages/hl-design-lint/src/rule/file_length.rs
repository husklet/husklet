use proc_macro2::Span;
use syn::spanned::Spanned;

use crate::{
    model::{Finding, Review, Severity},
    rule::Rule,
    source::Workspace,
    Result,
};

/// Rejects Rust files whose size obscures cohesive ownership boundaries.
pub struct FileLength;

impl Rule for FileLength {
    fn id(&self) -> &'static str {
        "file-length"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for source in workspace.sources() {
            if source.package == "hl-design-lint" {
                continue;
            }
            let lines = source.text.lines().count();
            if lines <= 500 {
                continue;
            }
            let span = source
                .syntax
                .items
                .first()
                .map(syn::Item::span)
                .unwrap_or_else(Span::call_site);
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
            review
                .metadata
                .push(("lines".to_owned(), lines.to_string()));
            review.metadata.push(("limit".to_owned(), "500".to_owned()));
            review.questions = vec![
                "Which independent responsibilities are mixed in this file?".to_owned(),
                "Does each extracted module have a precise domain name and dependency direction?"
                    .to_owned(),
                "Can the split be tested without relying on source-text assertions?".to_owned(),
            ];
            finding.review = Some(review);
            findings.push(finding);
        }
        Ok(findings)
    }
}
