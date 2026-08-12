use crate::{
    Result,
    model::{Finding, Severity},
    source::Workspace,
};

mod accessor;
mod arguments;
mod blocking;
mod boolean;
mod boundary;
mod c;
mod catchall;
mod ceremony;
mod command;
mod contract;
mod dependency;
mod duplicate;
mod empty;
mod environment;
mod escape;
mod folder;
mod function;
mod length;
mod model;
mod naming;
mod nesting;
mod object;
mod ownership;
mod placement;
mod receiver;
mod references;
mod result;
mod role;
mod safety;
mod shape;
mod state;
mod suite;
mod suite_path;
mod syntax;
mod toolkit;

pub use accessor::Bloat as AccessorBloat;
pub use arguments::ManualDispatch;
pub use blocking::AsyncBlocking;
pub use boolean::State as BooleanState;
pub use boundary::PathModules;
pub use c::{CallPolicy as CCallPolicy, Policy as CPolicy, Structure as CStructure};
pub use catchall::CatchAllModule;
pub use ceremony::CeremonialStructure;
pub use command::PlatformCommand;
pub use contract::BroadTrait;
pub use dependency::Direction as DependencyDirection;
pub use duplicate::Entity as DuplicateEntity;
pub use empty::Directory as EmptyDirectory;
pub use environment::Access as EnvironmentAccess;
pub use escape::Repository as RepositoryEscape;
pub use folder::SingleFileDirectory;
pub use function::FreeFunction;
pub use length::FileLength;
pub use model::Duplication as ModelDuplication;
pub use naming::StructNaming;
pub use nesting::MaximumNesting;
pub use object::GodObject;
pub use ownership::RuntimeTool;
pub use placement::IntegrationCandidate;
pub use receiver::Repetition as ReceiverRepetition;
pub use result::IgnoredResult;
pub use role::Suffix as SuffixRole;
pub use safety::Boundary as UnsafeBoundary;
pub use shape::{FileName, FolderNoun, ModulePrefix, PrefixDirectory, TestName};
pub use state::FiniteStateString;
pub use suite::{Dependency as TestDependency, Directory as TestDirectory};
pub use suite_path::KebabPath as TestSuiteKebabPath;
pub use toolkit::GuiToolkitLeakage;

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
