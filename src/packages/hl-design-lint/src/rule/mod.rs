use crate::{
    model::{Finding, Severity},
    source::Workspace,
    Result,
};

mod async_blocking;
mod boolean_state;
#[cfg(test)]
mod boolean_state_tests;
mod catch_all_module;
mod deep_control_flow;
mod dependency_direction;
mod duplicate_entity;
mod empty_directory;
mod environment_access;
mod file_length;
mod free_function;
mod gui_toolkit_leakage;
mod ignored_result;
mod platform_command;
mod receiver_repetition;
mod single_use;
mod struct_naming;
mod usage;

pub use async_blocking::AsyncBlocking;
pub use boolean_state::BooleanState;
pub use catch_all_module::CatchAllModule;
pub use deep_control_flow::DeepControlFlow;
pub use dependency_direction::DependencyDirection;
pub use duplicate_entity::DuplicateEntity;
pub use empty_directory::EmptyDirectory;
pub use environment_access::EnvironmentAccess;
pub use file_length::FileLength;
pub use free_function::FreeFunction;
pub use gui_toolkit_leakage::GuiToolkitLeakage;
pub use ignored_result::IgnoredResult;
pub use platform_command::PlatformCommand;
pub use receiver_repetition::ReceiverRepetition;
pub use single_use::SingleUse;
pub use struct_naming::StructNaming;

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
