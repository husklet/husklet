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
#[derive(Default)]
pub struct RuntimeTool {
    policy: crate::policy::OwnershipPolicy,
}

impl RuntimeTool {
    /// Creates a generic ownership rule.
    #[must_use]
    pub fn new(policy: crate::policy::OwnershipPolicy) -> Self {
        Self { policy }
    }
}

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
            .filter(|source| {
                self.policy.protected_domains.contains(&source.domain) && tool_name(&source.package, &self.policy)
            })
            .filter(|source| seen.insert(source.package.clone()))
            .map(|source| {
                let mut finding =
                    Finding::error(self.id(), source.package.clone(), source.location(source.syntax.span()));
                finding.message =
                    "repository audit and test tools do not belong to this protected infrastructure domain".into();
                finding.help = self.policy.destination_domain.as_ref().map_or_else(
                    || "move the package to a repository-owned tool domain".into(),
                    |domain| format!("move the package under `{domain}` or fold it into an existing tool application"),
                );
                finding
            })
            .collect())
    }
}

fn tool_name(name: &str, policy: &crate::policy::OwnershipPolicy) -> bool {
    policy.tool_names.iter().any(|value| value == name)
        || policy.tool_contains.iter().any(|value| name.contains(value))
        || policy.tool_suffixes.iter().any(|value| name.ends_with(value))
}
