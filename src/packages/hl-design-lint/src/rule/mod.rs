use crate::{
    Result,
    model::{Finding, Severity},
    source::Workspace,
};

mod c;
mod repository;
mod rust;
mod support;

pub use c::{
    Allocation as CAllocation, CallPolicy as CCallPolicy, Interface as CInterface, Policy as CPolicy,
    ResultUse as CResult, Safety as CSafety, Structure as CStructure, TestOnlyState as CTestOnlyState,
    analyzer::{AnalyzerConfig as CAnalyzerConfig, run as run_c_analyzers},
};
pub use repository::{
    CatchAllSourcePath, DependencyDirection, Documentation, EmptyDirectory, FileLength, FileName, FolderNoun,
    ModulePrefix, ParentName, PrefixDirectory, ProvisionalDiagnostic, RepositoryEscape, RuntimeTool,
    SingleFileDirectory, TestDependency, TestDirectory, TestName, TestSuiteKebabPath,
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
    /// The diagnostic identifiers this rule reports under, when they are finer than [`Rule::id`].
    ///
    /// A rule that reports one diagnostic leaves this empty and is summarized under its own id. A
    /// rule that shares one analysis across several independent budgets -- a C file's length and a
    /// C function's length are measured together but mean different things -- names them here, so
    /// the roll-up totals each budget separately instead of adding a file's excess lines to a
    /// function's. Every identifier a finding uses must appear, or its findings are summarized
    /// nowhere.
    fn diagnostics(&self) -> &'static [&'static str] {
        &[]
    }
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
