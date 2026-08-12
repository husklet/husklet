use crate::{
    Result,
    model::{Finding, Location, Review, Severity},
    rule::Rule,
    source::{Workspace, domain, package},
};

#[cfg(test)]
#[path = "test.rs"]
mod tests;

/// Reviews directories that add a namespace for one file.
pub struct SingleFileDirectory;

impl Rule for SingleFileDirectory {
    fn id(&self) -> &'static str {
        "single-file-directory"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        Ok(workspace
            .single_file_directories()
            .iter()
            .filter(|(directory, _)| !is_required_structure(directory))
            .map(|(directory, file)| finding(self.id(), directory, file))
            .collect())
    }
}

fn is_required_structure(directory: &std::path::Path) -> bool {
    let name = directory.file_name().and_then(|name| name.to_str());
    let cargo_package_source = name == Some("src") && directory.join("../Cargo.toml").is_file();
    let cargo_target = matches!(name, Some("bin" | "benches" | "examples" | "tests"));
    let rust_companion = (directory.join("test.rs").is_file() || directory.join("tests.rs").is_file())
        && directory
            .parent()
            .zip(name)
            .is_some_and(|(parent, name)| parent.join(format!("{name}.rs")).is_file());
    let fixture_boundary = matches!(name, Some("expected" | "golden"))
        && directory.components().any(|part| part.as_os_str() == "tests");
    cargo_package_source || cargo_target || rust_companion || fixture_boundary
}

fn finding(rule: &'static str, directory: &std::path::Path, file: &std::path::Path) -> Finding {
    let subject = directory
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("directory")
        .to_owned();
    let mut finding = Finding::error(
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
        file.file_name().and_then(|name| name.to_str()).unwrap_or("one file")
    );
    finding.help = "collapse the namespace into a file, or add cohesive children that justify the directory".to_owned();
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
        "Does this directory define a stable boundary that cannot be represented by a file?".to_owned(),
        "Should the file move to the parent as `<directory>.rs`?".to_owned(),
        "Which cohesive second child justifies retaining the namespace?".to_owned(),
    ];
    finding.review = Some(review);
    finding
}
