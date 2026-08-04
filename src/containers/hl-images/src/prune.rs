use std::collections::BTreeSet;

use crate::{Descriptor, GcReport};

/// Result of pruning graph records and their now-unreachable content.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GraphPruneReport {
    pub gc: GcReport,
    /// Graph records actually removed from the mutable workspace catalog.
    pub graphs_removed: Vec<PrunedGraph>,
}

impl std::ops::Deref for GraphPruneReport {
    type Target = GcReport;

    fn deref(&self) -> &Self::Target {
        &self.gc
    }
}

/// One descriptor-graph record removed by a prune transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrunedGraph {
    pub target: Descriptor,
    pub names: BTreeSet<String>,
}
