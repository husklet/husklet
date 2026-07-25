mod cases;
mod diagnostic;
mod markdown;

use crate::{
    model::{Finding, Summary},
    source::Workspace,
    Result,
};

pub use cases::Cases;
pub use diagnostic::Diagnostic;
pub use markdown::Markdown;

/// Receives format-neutral findings from the lint runner.
pub trait Reporter {
    /// Prepares output before rules run.
    fn begin(&mut self, _workspace: &Workspace) -> Result<()> {
        Ok(())
    }

    /// Emits one finding.
    fn finding(&mut self, finding: &Finding) -> Result<()>;

    /// Finalizes output after all rules run.
    fn finish(&mut self, summaries: &[Summary]) -> Result<()>;
}
