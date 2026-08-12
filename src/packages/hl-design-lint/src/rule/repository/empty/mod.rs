use crate::{
    Result,
    model::{Finding, Location, Review, Severity},
    rule::Rule,
    source::{Workspace, domain, package},
};

#[cfg(test)]
#[path = "test.rs"]
mod tests;

/// Rejects repository structure that names a directory without owned content.
///
/// Placeholder files and configured generated or externally owned subtrees do
/// not make a directory substantive. Excluded subtrees are not inspected.
pub struct Directory;

impl Rule for Directory {
    fn id(&self) -> &'static str {
        "empty-directory"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        Ok(workspace
            .empty_directories()
            .iter()
            .map(|path| {
                let subject = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("directory")
                    .to_owned();
                let mut finding = Finding::error(
                    self.id(),
                    subject,
                    Location {
                        path: path.clone(),
                        line: 1,
                        column: 1,
                        source: String::new(),
                    },
                );
                finding.message = format!("directory `{}` has no content", path.display());
                finding.help =
                    "remove the directory; create it with its first cohesive file when the concept exists".to_owned();
                let mut review = Review::error();
                review.metadata.push(("domain".to_owned(), domain(path)));
                review.metadata.push((
                    "package".to_owned(),
                    package(path).unwrap_or_else(|| "repository".to_owned()),
                ));
                review.questions = vec![
                    "Is this directory obsolete and safe to remove?".to_owned(),
                    "If content is expected, which cohesive entity owns the first file?".to_owned(),
                ];
                finding.review = Some(review);
                finding
            })
            .collect())
    }
}
