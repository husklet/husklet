//! A portable, toolkit-neutral component library.
//!
//! The library owns three things and nothing else: a retained node tree with
//! incremental mutation, a closed styling vocabulary, and windowed data sources
//! for collection components. It has no dependencies, no toolkit imports, and
//! no knowledge of the application embedding it, so an interface described here
//! can be rendered by any adapter and driven over any transport.
//!
//! A producer emits [`Frame`]s of [`Patch`]es; [`Tree`] validates and retains
//! them and forwards them to a [`Renderer`]. Interaction returns as [`Event`]s.
//!
//! ```
//! use hl_gui::{Prop, PropValue, Surface, Tag};
//!
//! let mut surface = Surface::new();
//! let button = surface.create(Tag::Button);
//! surface.set(button, Prop::Label, PropValue::text("Restart"));
//! surface.append(hl_gui::NodeId::ROOT, button);
//! let frame = surface.frame();
//! assert_eq!(frame.sequence, 1);
//! ```

mod builder;
mod component;
mod data;
mod dialog;
mod element;
mod identity;
mod node;
mod render;
mod size;
mod style;

pub use builder::Surface;
pub use component::{
    CoverageLine, CoverageSource, CoverageView, DependencyCycle, DependencyEdge, DependencyNode, DependencyRelation,
    DependencySource, DependencyState, FlameFrame, HexSource, HexView, HttpMethod, Instruction, MemoryRegion,
    NetworkPhase, NetworkPhaseKind, NetworkRequest, NetworkSource, QueryMetric, QueryMetricKind, QueryNodeState,
    QueryOperator, QueryPlanNode, QueryPlanSource, TestCase, TestStatus, TimelineEvent,
};
pub use data::{
    Cell, CollectionEdit, Column, Lookup, RequestId, Row, RowCache, RowRange, RowRequest, RowWindow, Sort, SourceId,
    SourceMutation, Version,
};
pub use dialog::{Action, Dialog, Role};
pub use element::{Element, Reconciliation};
pub use identity::{Identities, NodeId};
pub use node::{
    Choice, EventId, Fault, Frame, Handler, Node, Orientation, Patch, Prop, PropValue, Tag, Tree, TreeError, Trigger,
};
pub use render::{CollectionSelection, Event, Events, PointerPhase, Renderer, SelectedRow};
pub use size::ByteSize;
pub use style::{Align, Bounds, Density, Edges, Length, Rgb, Scale, Theme, Token, Tone, Variant};

/// Maximum Unicode characters a [`Tag::LogView`] retains.
///
/// A log's `Value` patches are append-only deltas. Renderers discard the oldest
/// characters beyond this bound so a long-running operational surface cannot
/// grow host memory without limit.
pub const LOG_VIEW_CHARACTER_LIMIT: i32 = 4_096;

/// Maximum number of source bytes a [`Tag::HexView`] renders.
pub const HEX_VIEW_BYTE_LIMIT: usize = 4_096;

/// Maximum finite samples retained by a [`Tag::Sparkline`].
pub const SPARKLINE_SAMPLE_LIMIT: usize = 64;

/// Maximum number of profile frames retained by a [`Tag::FlameGraph`].
pub const FLAME_GRAPH_FRAME_LIMIT: usize = 64;

/// Maximum number of address regions retained by a [`Tag::MemoryMap`].
pub const MEMORY_MAP_REGION_LIMIT: usize = 128;

/// Maximum number of decoded instructions retained by a [`Tag::DisassemblyView`].
pub const DISASSEMBLY_INSTRUCTION_LIMIT: usize = 256;

/// Maximum number of chronological events retained by a [`Tag::TimelineView`].
pub const TIMELINE_EVENT_LIMIT: usize = 256;

pub const TEST_REPORT_CASE_LIMIT: usize = 256;
pub const TEST_REPORT_FAILURE_CHARACTER_LIMIT: usize = 512;
pub const COVERAGE_VIEW_LINE_LIMIT: usize = 512;
pub const COVERAGE_VIEW_SOURCE_CHARACTER_LIMIT: usize = 512;
pub const NETWORK_WATERFALL_REQUEST_LIMIT: usize = 32;
pub const NETWORK_WATERFALL_PHASE_LIMIT: usize = 6;
pub const NETWORK_WATERFALL_TEXT_LIMIT: usize = 160;
pub const NETWORK_WATERFALL_TIME_LIMIT_US: u64 = 86_400_000_000;
pub const NETWORK_WATERFALL_PHASE_TIME_LIMIT_US: u64 = 3_600_000_000;
pub const NETWORK_WATERFALL_BYTE_LIMIT: u64 = 1 << 40;
pub const DEPENDENCY_GRAPH_NODE_LIMIT: usize = 32;
pub const DEPENDENCY_GRAPH_EDGE_LIMIT: usize = 128;
pub const DEPENDENCY_GRAPH_CYCLE_LIMIT: usize = 8;
pub const DEPENDENCY_GRAPH_CYCLE_MEMBER_LIMIT: usize = 6;
pub const QUERY_PLAN_NODE_LIMIT: usize = 36;
pub const QUERY_PLAN_METRIC_LIMIT: usize = 5;
pub const QUERY_PLAN_DEPTH_LIMIT: usize = 12;
