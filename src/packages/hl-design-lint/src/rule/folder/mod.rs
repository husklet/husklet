use crate::{
    model::{Finding, Location, Review, Severity},
    rule::Rule,
    source::{domain, package, Workspace},
    Result,
};

#[cfg(test)]
mod tests;

/// Reviews directories that add a namespace for one file.
pub struct SingleFileDirectory;

impl Rule for SingleFileDirectory {
    fn id(&self) -> &'static str {
        "single-file-directory"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        Ok(workspace
            .single_file_directories()
            .iter()
            .map(|(directory, file)| finding(self.id(), directory, file))
            .collect())
    }
}

fn finding(rule: &'static str, directory: &std::path::Path, file: &std::path::Path) -> Finding {
    let subject = directory
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("directory")
        .to_owned();
    let mut finding = Finding::warning(
        rule,
        subject,
        Location {
            path: directory.to_owned(),
            line: 1,
            column: 1,
            source: String::new(),
        },
    );
    finding.message = format!(
        "directory `{}` contains only `{}`",
        directory.display(),
        file.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("one file")
    );
    finding.help =
        "collapse the namespace into a file, or add cohesive children that justify the directory"
            .to_owned();
    let mut review = Review::error();
    review.metadata = vec![
        ("domain".to_owned(), domain(directory)),
        (
            "package".to_owned(),
            package(file).unwrap_or_else(|| "repository".to_owned()),
        ),
        ("only file".to_owned(), file.display().to_string()),
    ];
    review.questions = vec![
        "Does this directory define a stable boundary that cannot be represented by a file?"
            .to_owned(),
        "Should the file move to the parent as `<directory>.rs`?".to_owned(),
        "Which cohesive second child justifies retaining the namespace?".to_owned(),
    ];
    finding.review = Some(review);
    finding
}
