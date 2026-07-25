#[path = "report/batch.rs"]
mod batch;
#[path = "report/schema.rs"]
mod schema;
#[path = "report/store.rs"]
mod store;

#[path = "report/tests.rs"]
pub(crate) mod tests;

pub use batch::ScenarioBatch;
pub type ScenarioAttempt = batch::ScenarioAttempt;
pub use schema::{
    Attempt, BatchMetadata, BatchReport, ScenarioKey, ScenarioOutcome, Status, WorkflowAttempt,
    WorkflowKey, WorkflowOutcome,
};
pub use store::Store;
