use std::collections::BTreeSet;
use syn::spanned::Spanned;

use crate::{
    Result,
    model::{Finding, Severity},
    rule::Rule,
    source::Workspace,
};

#[cfg(test)]
#[path = "test.rs"]
mod tests;

/// Keeps repository audit and test applications out of the runtime domain.
pub struct RuntimeTool;

impl Rule for RuntimeTool {
    fn id(&self) -> &'static str {
        "runtime-tool-ownership"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut seen = BTreeSet::new();
        Ok(workspace
            .sources()
            .iter()
            .filter(|source| source.domain == "runtime" && tool_name(&source.package))
            .filter(|source| seen.insert(source.package.clone()))
            .map(|source| {
                let mut finding =
                    Finding::error(self.id(), source.package.clone(), source.location(source.syntax.span()));
                finding.message = "repository audit and test tools are applications, not runtime infrastructure".into();
                finding.help =
                    "move the package under src/apps or fold the behavior into the testing application".into();
                finding
            })
            .collect())
    }
}

fn tool_name(name: &str) -> bool {
    name == "testing"
        || name.contains("audit")
        || name.ends_with("-test")
        || name.ends_with("-tests")
        || name.ends_with("-tool")
        || name.ends_with("-tools")
}
