#[path = "report/legacy.rs"]
mod legacy;
#[path = "report/schema.rs"]
mod schema;
#[path = "report/store.rs"]
mod store;

#[path = "report/tests.rs"]
pub(crate) mod tests;

pub use legacy::LegacyBatch;
pub type LegacyAttempt = legacy::LegacyAttempt;
pub use schema::{
    Attempt, BatchMetadata, BatchReport, ScenarioKey, ScenarioOutcome, Status, WorkflowAttempt,
    WorkflowKey, WorkflowOutcome,
};
pub use store::Store;
