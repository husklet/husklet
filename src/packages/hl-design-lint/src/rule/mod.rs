use crate::{
    model::{Finding, Severity},
    source::Workspace,
    Result,
};

mod accessor;
mod blocking;
mod boolean;
mod catchall;
mod ceremony;
mod command;
mod contract;
mod dependency;
mod duplicate;
mod empty;
mod environment;
mod folder;
mod function;
mod length;
mod model;
mod naming;
mod nesting;
mod object;
mod receiver;
mod references;
mod result;
mod single;
mod state;
mod syntax;
mod toolkit;

pub use accessor::AccessorBloat;
pub use blocking::AsyncBlocking;
pub use boolean::BooleanState;
pub use catchall::CatchAllModule;
pub use ceremony::CeremonialStructure;
pub use command::PlatformCommand;
pub use contract::BroadTrait;
pub use dependency::DependencyDirection;
pub use duplicate::DuplicateEntity;
pub use empty::EmptyDirectory;
pub use environment::EnvironmentAccess;
pub use folder::SingleFileDirectory;
pub use function::FreeFunction;
pub use length::FileLength;
pub use model::ModelDuplication;
pub use naming::StructNaming;
pub use nesting::DeepControlFlow;
pub use object::GodObject;
pub use receiver::ReceiverRepetition;
pub use result::IgnoredResult;
pub use single::SingleUse;
pub use state::FiniteStateString;
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
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// Appends a rule in execution order.
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
