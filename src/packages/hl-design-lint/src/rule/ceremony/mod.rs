use crate::{
    Result,
    model::{Finding, Severity},
    rule::Rule,
    source::Workspace,
};

mod marker;
mod namespace;
mod wrapper;

#[cfg(test)]
#[path = "test.rs"]
mod tests;

pub(super) const ID: &str = "ceremonial-structure";

/// Reviews structure that adds navigation without a contract, boundary, or invariant.
pub struct CeremonialStructure;

impl Rule for CeremonialStructure {
    fn id(&self) -> &'static str {
        ID
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = namespace::findings(workspace);
        findings.extend(marker::findings(workspace));
        findings.extend(wrapper::findings(workspace));
        findings.sort_by(|left, right| {
            left.location
                .path
                .cmp(&right.location.path)
                .then(left.location.line.cmp(&right.location.line))
                .then(left.subject.cmp(&right.subject))
        });
        Ok(findings)
    }
}
