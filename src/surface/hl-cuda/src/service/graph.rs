//! `cudaGraphCreate` / `cudaGraphAdd*Node` / `cudaGraphInstantiate` / `cudaGraphLaunch` /
//! `cudaGraphExecDestroy` — the CUDA graph service.
//!
//! Building a graph mutates a [`Graph`] template (no IR emitted — a template is host bookkeeping);
//! instantiating freezes it into an [`ExecGraph`]; launching **replays** each node through the identical
//! eager service ([`transfer`]/[`launch`]), so a graph launch lowers exactly the `Cmd` stream the eager
//! sequence would. That is the property the demo asserts: graph-replayed output == eager-sequence output,
//! bit-exact.

use crate::model::context::CudaContext;
use crate::model::device::DevicePtr;
use crate::model::graph::{ExecGraph, Graph, GraphNode};
use crate::model::module::{Function, KernelArg};
use crate::service::{launch, transfer};
use hl_gpu::{CommandSink, Result};

/// `cudaGraphCreate(&graph, 0)` — a fresh, empty graph template.
pub fn graph_create() -> Graph {
    Graph::new()
}

/// `cudaGraphAddMemcpyNode(..., HostToDevice)` — append an H2D copy node. Returns the node index.
pub fn add_memcpy_htod_node(graph: &mut Graph, dst: DevicePtr, data: &[u8]) -> usize {
    graph.add(GraphNode::MemcpyHtoD {
        dst,
        data: data.to_vec(),
    })
}

/// `cudaGraphAddMemsetNode(...)` — append a memset node with the already-expanded byte `pattern`.
pub fn add_memset_node(graph: &mut Graph, dst: DevicePtr, pattern: &[u8]) -> usize {
    graph.add(GraphNode::Memset {
        dst,
        pattern: pattern.to_vec(),
    })
}

/// `cudaGraphAddKernelNode(...)` — append a kernel-launch node. Returns the node index.
pub fn add_kernel_node(
    graph: &mut Graph,
    func: Function,
    grid: (u32, u32, u32),
    block: (u32, u32, u32),
    args: Vec<KernelArg>,
) -> usize {
    graph.add(GraphNode::Kernel {
        func,
        grid,
        block,
        args,
    })
}

/// `cudaGraphInstantiate(&exec, graph, ...)` — freeze the template into an executable graph. Cheap here
/// (a node-list snapshot); a real driver would resolve dependencies + pre-build launch state.
impl Graph {
    pub fn instantiate(&self) -> ExecGraph {
        ExecGraph {
            nodes: self.nodes.clone(),
        }
    }
}

/// `cudaGraphLaunch(exec, stream)` — replay every node in order through its eager service. A kernel node
/// lowers through [`launch::launch`] exactly as an eager `cuLaunchKernel` would, so the graph computes the
/// same result. Idempotent: relaunching with the same inputs reproduces the same output.
pub fn launch_graph(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    exec: &ExecGraph,
) -> Result<()> {
    let _s = hl_log::hl_span!(hl_log::tag::CUDA, "graph_launch");
    for node in &exec.nodes {
        match node {
            GraphNode::MemcpyHtoD { dst, data } => {
                transfer::memcpy_htod(ctx, sink, *dst, data)?;
            }
            GraphNode::Memset { dst, pattern } => {
                transfer::memset(ctx, sink, *dst, pattern)?;
            }
            GraphNode::Kernel {
                func,
                grid,
                block,
                args,
            } => {
                launch::launch(ctx, sink, *func, *grid, *block, args)?;
            }
        }
    }
    hl_log::hl_count!(hl_log::tag::CUDA, "graph_launches");
    Ok(())
}
