use crate::{
    Result,
    model::{Finding, Severity},
    source::Workspace,
};

mod c;
mod repository;
mod rust;
mod support;

pub use c::{CallPolicy as CCallPolicy, Policy as CPolicy, Structure as CStructure};
pub use repository::{
    CatchAllSourcePath, DependencyDirection, Documentation, EmptyDirectory, FileLength, FileName, FolderNoun,
    ModulePrefix, ParentName, PrefixDirectory, RepositoryEscape, RuntimeTool, SingleFileDirectory, TestDependency,
    TestDirectory, TestName, TestSuiteKebabPath,
};
pub use rust::{
    AccessorBloat, AsyncBlocking, BooleanState, BroadTrait, CatchAllModule, CeremonialStructure, ConstructorOwnership,
    DuplicateEntity, EnvironmentAccess, FiniteStateString, FreeFunction, GodObject, GuiToolkitLeakage, IgnoredResult,
    IntegrationCandidate, ManualDispatch, MaximumNesting, ModelDuplication, PathModules, PlatformCommand,
    ReceiverRepetition, StructNaming, SuffixRole, UnsafeBoundary,
};

/// One independently executable design check.
pub trait Rule {
    /// Returns the stable diagnostic identifier.
    fn id(&self) -> &'static str;
    /// Returns the severity assigned to active findings.
    fn severity(&self) -> Severity;
    /// Analyzes the parsed workspace.
    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>>;
}

/// Ordered collection of lint rules.
pub struct Registry {
    rules: Vec<Box<dyn Rule>>,
}

impl Registry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// Appends a rule in execution order.
    #[must_use]
    pub fn register(mut self, rule: impl Rule + 'static) -> Self {
        self.rules.push(Box::new(rule));
        self
    }

    /// Iterates over registered rules in execution order.
    pub fn rules(&self) -> impl Iterator<Item = &dyn Rule> {
        self.rules.iter().map(Box::as_ref)
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs, path::Path};

    #[test]
    fn rule_tree_has_only_language_repository_and_support_domains() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/rule");
        let entries = fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            entries,
            BTreeSet::from([
                "c".into(),
                "mod.rs".into(),
                "repository".into(),
                "rust".into(),
                "support".into()
            ])
        );
    }
}
