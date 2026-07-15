//! CUDA **graphs** — the explicit-build model of `cudaGraphCreate` / `cudaGraphAdd*Node` /
//! `cudaGraphInstantiate` / `cudaGraphLaunch`.
//!
//! A CUDA graph is a captured DAG of operations (memcpies, memsets, kernel launches) that is built once,
//! instantiated into an executable graph, and then launched repeatedly with no per-op driver overhead. The
//! executor here is synchronous and the demo graphs are linear sequences, so a graph is modeled as an
//! **ordered node list**: [`Graph`] is the mutable template you add nodes to, [`ExecGraph`] is the frozen
//! executable snapshot [`cudaGraphInstantiate`] produces, and a launch (in [`crate::service::graph`])
//! **replays each node through the very same service the eager API uses** — a kernel node lowers through
//! [`crate::service::launch::launch`], byte-for-byte identical to an eager `cuLaunchKernel`. There is no
//! second execution path, so a graph launch computes exactly what running the nodes eagerly would.

use crate::model::device::DevicePtr;
use crate::model::module::{Function, KernelArg};

/// One node in a CUDA graph. Each variant carries exactly the parameters its eager service call needs, so
/// replay is a direct re-invocation of that service.
#[derive(Clone, PartialEq, Debug)]
pub enum GraphNode {
    /// `cudaGraphAddMemcpyNode` (host→device) — write `data` into the buffer backing `dst`.
    MemcpyHtoD { dst: DevicePtr, data: Vec<u8> },
    /// `cudaGraphAddMemsetNode` — fill the buffer backing `dst` with the expanded byte `pattern`.
    Memset { dst: DevicePtr, pattern: Vec<u8> },
    /// `cudaGraphAddKernelNode` — a kernel launch with a fixed grid/block and argument list.
    Kernel { func: Function, grid: (u32, u32, u32), block: (u32, u32, u32), args: Vec<KernelArg> },
}

/// A buildable CUDA graph (`cudaGraph_t`): the ordered node template. Nodes are appended in dependency
/// order (the linear-sequence subset the demos exercise).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Graph {
    pub nodes: Vec<GraphNode>,
}

impl Graph {
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    /// Append a node, returning its index (the `cudaGraphNode_t` analogue).
    pub fn add(&mut self, node: GraphNode) -> usize {
        let idx = self.nodes.len();
        self.nodes.push(node);
        idx
    }
}

/// An instantiated, executable CUDA graph (`cudaGraphExec_t`): a frozen snapshot of the template's nodes.
/// Produced by `cudaGraphInstantiate`; launched (replayed) by [`crate::service::graph::launch_graph`].
#[derive(Clone, PartialEq, Debug)]
pub struct ExecGraph {
    pub nodes: Vec<GraphNode>,
}
