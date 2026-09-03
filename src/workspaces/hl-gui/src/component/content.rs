//! Constructors for the components that show something: marks, imagery,
//! feedback and long-form content.

use std::fmt::Write as _;

use crate::element::Element;
use crate::node::{Prop, PropValue, Tag};

/// The provenance of bytes presented by a [`HexView`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HexSource<'a> {
    /// The slice is the complete value.
    Exact(&'a [u8]),
    /// The slice is a prefix of a value whose complete byte length is known.
    Bounded { prefix: &'a [u8], total_bytes: usize },
}

/// A bounded, deterministic binary projection for [`Element::hex_view`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HexView(String);

/// One labelled frame in a sampled profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlameFrame {
    label: String,
    samples: u64,
}

impl FlameFrame {
    /// Makes a visible frame. Empty labels and zero samples are omitted.
    #[must_use]
    pub fn new(label: impl Into<String>, samples: u64) -> Option<Self> {
        let label = label.into().replace(['\t', '\n', '\r'], " ");
        (!label.trim().is_empty() && samples > 0).then_some(Self { label, samples })
    }
}

/// One non-empty virtual address region in a process map.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryRegion {
    start: u64,
    end: u64,
    permissions: String,
    mapping: String,
}

impl MemoryRegion {
    /// Makes a region. Reversed/empty ranges and malformed permissions are omitted.
    #[must_use]
    pub fn new(start: u64, end: u64, permissions: impl Into<String>, mapping: impl Into<String>) -> Option<Self> {
        let permissions = permissions.into();
        let valid_permissions = !permissions.is_empty()
            && permissions.len() <= 4
            && permissions
                .bytes()
                .all(|byte| matches!(byte, b'r' | b'w' | b'x' | b'p' | b's' | b'-'));
        (start < end && valid_permissions).then_some(Self {
            start,
            end,
            permissions,
            mapping: mapping.into().replace(['\t', '\n', '\r'], " "),
        })
    }
}

/// One decoded machine instruction with its original bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Instruction {
    address: u64,
    bytes: Vec<u8>,
    mnemonic: String,
    operands: String,
}

impl Instruction {
    /// Makes an instruction. Empty or implausibly long encodings are omitted.
    #[must_use]
    pub fn new(
        address: u64,
        bytes: impl Into<Vec<u8>>,
        mnemonic: impl Into<String>,
        operands: impl Into<String>,
    ) -> Option<Self> {
        let bytes = bytes.into();
        let mnemonic = mnemonic.into().replace(['\t', '\n', '\r'], " ");
        let operands = operands.into().replace(['\t', '\n', '\r'], " ");
        (!bytes.is_empty() && bytes.len() <= 16 && !mnemonic.trim().is_empty()).then_some(Self {
            address,
            bytes,
            mnemonic,
            operands,
        })
    }
}

/// One timestamped event in a developer chronology.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimelineEvent {
    timestamp_ms: i64,
    category: String,
    label: String,
    detail: String,
}

impl TimelineEvent {
    /// Makes a readable event; blank labels are omitted and wire separators are neutralized.
    #[must_use]
    pub fn new(
        timestamp_ms: i64,
        category: impl Into<String>,
        label: impl Into<String>,
        detail: impl Into<String>,
    ) -> Option<Self> {
        fn clean(value: String) -> String {
            value.replace(['\t', '\n', '\r'], " ")
        }
        let category = clean(category.into());
        let label = clean(label.into());
        let detail = clean(detail.into());
        (!label.trim().is_empty()).then_some(Self {
            timestamp_ms,
            category,
            label,
            detail,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TestStatus {
    Passed,
    Failed,
    Skipped,
}
impl TestStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestCase {
    suite: String,
    name: String,
    status: TestStatus,
    duration_ms: u64,
    failure: String,
}
impl TestCase {
    #[must_use]
    pub fn new(
        suite: impl Into<String>,
        name: impl Into<String>,
        status: TestStatus,
        duration_ms: u64,
        failure: impl Into<String>,
    ) -> Option<Self> {
        fn clean(value: String) -> String {
            value.replace(['\t', '\n', '\r'], " ")
        }
        let suite = clean(suite.into());
        let name = clean(name.into());
        let failure = clean(failure.into())
            .chars()
            .take(crate::TEST_REPORT_FAILURE_CHARACTER_LIMIT)
            .collect();
        (!suite.trim().is_empty() && !name.trim().is_empty()).then_some(Self {
            suite,
            name,
            status,
            duration_ms,
            failure,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoverageLine {
    line: u32,
    hits: u64,
    source: String,
}
impl CoverageLine {
    #[must_use]
    pub fn new(line: u32, hits: u64, source: impl Into<String>) -> Option<Self> {
        let source = source
            .into()
            .replace(['\t', '\n', '\r'], " ")
            .chars()
            .take(crate::COVERAGE_VIEW_SOURCE_CHARACTER_LIMIT)
            .collect();
        (line > 0).then_some(Self { line, hits, source })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum CoverageSource<'a> {
    Exact(&'a [CoverageLine]),
    Bounded {
        prefix: &'a [CoverageLine],
        total_lines: usize,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoverageView(String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpMethod {
    Delete,
    Get,
    Head,
    Options,
    Patch,
    Post,
    Put,
}
impl HttpMethod {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "DELETE",
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
            Self::Patch => "PATCH",
            Self::Post => "POST",
            Self::Put => "PUT",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkPhaseKind {
    Dns,
    Connect,
    Tls,
    Request,
    Wait,
    Download,
}
impl NetworkPhaseKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::Connect => "connect",
            Self::Tls => "tls",
            Self::Request => "request",
            Self::Wait => "wait",
            Self::Download => "download",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkPhase {
    kind: NetworkPhaseKind,
    offset_us: u64,
    duration_us: u64,
}
impl NetworkPhase {
    #[must_use]
    pub fn new(kind: NetworkPhaseKind, offset_us: u64, duration_us: u64) -> Option<Self> {
        (duration_us > 0
            && duration_us <= crate::NETWORK_WATERFALL_PHASE_TIME_LIMIT_US
            && offset_us
                .checked_add(duration_us)
                .is_some_and(|end| end <= crate::NETWORK_WATERFALL_TIME_LIMIT_US))
        .then_some(Self {
            kind,
            offset_us,
            duration_us,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkRequest {
    method: HttpMethod,
    url: String,
    start_us: u64,
    duration_us: u64,
    status: Option<u16>,
    bytes: u64,
    detail: String,
    phases: Vec<NetworkPhase>,
}
impl NetworkRequest {
    #[must_use]
    pub fn new(
        method: HttpMethod,
        url: impl Into<String>,
        start_us: u64,
        duration_us: u64,
        status: Option<u16>,
        bytes: u64,
        detail: impl Into<String>,
        phases: impl IntoIterator<Item = NetworkPhase>,
    ) -> Option<Self> {
        fn clean(value: String) -> String {
            value
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(crate::NETWORK_WATERFALL_TEXT_LIMIT)
                .collect()
        }
        let url = clean(url.into());
        let detail = clean(detail.into());
        let phases: Vec<_> = phases
            .into_iter()
            .take(crate::NETWORK_WATERFALL_PHASE_LIMIT + 1)
            .collect();
        let timing = duration_us > 0
            && start_us
                .checked_add(duration_us)
                .is_some_and(|end| end <= crate::NETWORK_WATERFALL_TIME_LIMIT_US);
        let phase_shape = phases.len() <= crate::NETWORK_WATERFALL_PHASE_LIMIT
            && phases.iter().all(|p| p.offset_us + p.duration_us <= duration_us)
            && phases
                .windows(2)
                .all(|p| p[0].offset_us + p[0].duration_us <= p[1].offset_us);
        (!url.trim().is_empty()
            && timing
            && status.is_none_or(|s| (100..=599).contains(&s))
            && bytes <= crate::NETWORK_WATERFALL_BYTE_LIMIT
            && phase_shape)
            .then_some(Self {
                method,
                url,
                start_us,
                duration_us,
                status,
                bytes,
                detail,
                phases,
            })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum NetworkSource<'a> {
    Exact(&'a [NetworkRequest]),
    Bounded {
        prefix: &'a [NetworkRequest],
        total_requests: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DependencyState {
    Resolved,
    Missing,
    Conflict,
}
impl DependencyState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Missing => "missing",
            Self::Conflict => "conflict",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DependencyRelation {
    Runtime,
    Development,
    Optional,
    Peer,
    Build,
}
impl DependencyRelation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Development => "development",
            Self::Optional => "optional",
            Self::Peer => "peer",
            Self::Build => "build",
        }
    }
}
fn dep_clean(value: impl Into<String>, limit: usize) -> String {
    value
        .into()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(limit)
        .collect()
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyNode {
    id: String,
    label: String,
    version: String,
    state: DependencyState,
    detail: String,
}
impl DependencyNode {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        version: impl Into<String>,
        state: DependencyState,
        detail: impl Into<String>,
    ) -> Option<Self> {
        let id = dep_clean(id, 40);
        let label = dep_clean(label, 120);
        let version = dep_clean(version, 64);
        let detail = dep_clean(detail, 160);
        (!id.trim().is_empty() && !label.trim().is_empty()).then_some(Self {
            id,
            label,
            version,
            state,
            detail,
        })
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyEdge {
    source: String,
    target: String,
    relation: DependencyRelation,
    requirement: String,
}
impl DependencyEdge {
    #[must_use]
    pub fn new(
        source: impl Into<String>,
        target: impl Into<String>,
        relation: DependencyRelation,
        requirement: impl Into<String>,
    ) -> Option<Self> {
        let source = dep_clean(source, 40);
        let target = dep_clean(target, 40);
        let requirement = dep_clean(requirement, 96);
        (!source.trim().is_empty() && !target.trim().is_empty()).then_some(Self {
            source,
            target,
            relation,
            requirement,
        })
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyCycle {
    members: Vec<String>,
}
impl DependencyCycle {
    #[must_use]
    pub fn new(members: impl IntoIterator<Item = impl Into<String>>) -> Option<Self> {
        let members: Vec<_> = members.into_iter().map(|v| dep_clean(v, 40)).collect();
        let unique: std::collections::BTreeSet<_> = members.iter().collect();
        (!members.is_empty()
            && members.len() <= crate::DEPENDENCY_GRAPH_CYCLE_MEMBER_LIMIT
            && unique.len() == members.len()
            && members.iter().all(|v| !v.trim().is_empty()))
        .then_some(Self { members })
    }
}
#[derive(Clone, Copy, Debug)]
pub enum DependencySource<'a> {
    Exact {
        nodes: &'a [DependencyNode],
        edges: &'a [DependencyEdge],
        cycles: &'a [DependencyCycle],
    },
    Bounded {
        nodes: &'a [DependencyNode],
        total_nodes: usize,
        edges: &'a [DependencyEdge],
        total_edges: usize,
        cycles: &'a [DependencyCycle],
        total_cycles: usize,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryOperator {
    TableScan,
    IndexScan,
    IndexOnlyScan,
    BitmapScan,
    Filter,
    NestedLoop,
    HashJoin,
    MergeJoin,
    Hash,
    Sort,
    Aggregate,
    Group,
    Limit,
    Materialize,
    CteScan,
    SubqueryScan,
    Append,
    Result,
    Insert,
    Update,
    Delete,
}
impl QueryOperator {
    fn as_str(self) -> &'static str {
        match self {
            Self::TableScan => "table_scan",
            Self::IndexScan => "index_scan",
            Self::IndexOnlyScan => "index_only_scan",
            Self::BitmapScan => "bitmap_scan",
            Self::Filter => "filter",
            Self::NestedLoop => "nested_loop",
            Self::HashJoin => "hash_join",
            Self::MergeJoin => "merge_join",
            Self::Hash => "hash",
            Self::Sort => "sort",
            Self::Aggregate => "aggregate",
            Self::Group => "group",
            Self::Limit => "limit",
            Self::Materialize => "materialize",
            Self::CteScan => "cte_scan",
            Self::SubqueryScan => "subquery_scan",
            Self::Append => "append",
            Self::Result => "result",
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryNodeState {
    Normal,
    Hot,
    EstimateMismatch,
    Spill,
}
impl QueryNodeState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Hot => "hot",
            Self::EstimateMismatch => "estimate_mismatch",
            Self::Spill => "spill",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum QueryMetricKind {
    EstimatedRows,
    ActualRows,
    Cost,
    DurationUs,
    Loops,
}
impl QueryMetricKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::EstimatedRows => "estimated_rows",
            Self::ActualRows => "actual_rows",
            Self::Cost => "cost",
            Self::DurationUs => "duration_us",
            Self::Loops => "loops",
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct QueryMetric {
    kind: QueryMetricKind,
    value: f64,
}
impl QueryMetric {
    #[must_use]
    pub fn new(kind: QueryMetricKind, value: f64) -> Option<Self> {
        (value.is_finite()
            && value >= 0.0
            && (!matches!(kind, QueryMetricKind::DurationUs) || value <= 86_400_000_000.0)
            && (!matches!(kind, QueryMetricKind::Loops) || value <= 1_000_000_000.0))
            .then_some(Self { kind, value })
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct QueryPlanNode {
    id: String,
    operator: QueryOperator,
    label: String,
    relation: String,
    state: QueryNodeState,
    detail: String,
    metrics: Vec<QueryMetric>,
    children: Vec<Self>,
}
impl QueryPlanNode {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        operator: QueryOperator,
        label: impl Into<String>,
        relation: impl Into<String>,
        state: QueryNodeState,
        detail: impl Into<String>,
        metrics: impl IntoIterator<Item = QueryMetric>,
        children: impl IntoIterator<Item = Self>,
    ) -> Option<Self> {
        fn clean(v: String, n: usize) -> String {
            v.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(n)
                .collect()
        }
        let id = clean(id.into(), 40);
        let label = clean(label.into(), 120);
        let relation = clean(relation.into(), 120);
        let detail = clean(detail.into(), 160);
        let metrics: Vec<_> = metrics.into_iter().collect();
        let unique: std::collections::BTreeSet<_> = metrics.iter().map(|m| m.kind).collect();
        (!id.trim().is_empty() && !label.trim().is_empty() && metrics.len() <= 5 && unique.len() == metrics.len())
            .then_some(Self {
                id,
                operator,
                label,
                relation,
                state,
                detail,
                metrics,
                children: children.into_iter().collect(),
            })
    }
}
#[derive(Clone, Copy, Debug)]
pub enum QueryPlanSource<'a> {
    Exact(&'a [QueryPlanNode]),
    Bounded {
        prefix: &'a [QueryPlanNode],
        total_nodes: usize,
    },
}
impl CoverageView {
    #[must_use]
    pub fn new(source: CoverageSource<'_>) -> Self {
        let (lines, total) = match source {
            CoverageSource::Exact(lines) => (lines, lines.len()),
            CoverageSource::Bounded { prefix, total_lines } => (prefix, total_lines.max(prefix.len())),
        };
        let shown = lines.len().min(crate::COVERAGE_VIEW_LINE_LIMIT);
        let mut value = lines[..shown]
            .iter()
            .map(|line| format!("{}\t{}\t{}", line.line, line.hits, line.source))
            .collect::<Vec<_>>()
            .join("\n");
        if total > shown {
            if !value.is_empty() {
                value.push('\n');
            }
            value.push_str(&format!("…\t\t… showing {shown} of {total} lines …"));
        }
        Self(value)
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl HexView {
    /// Formats 16-byte rows without ever inspecting more than the public limit.
    #[must_use]
    pub fn new(source: HexSource<'_>) -> Self {
        let (bytes, total) = match source {
            HexSource::Exact(bytes) => (bytes, bytes.len()),
            HexSource::Bounded { prefix, total_bytes } => (prefix, total_bytes.max(prefix.len())),
        };
        let shown = bytes.len().min(crate::HEX_VIEW_BYTE_LIMIT);
        let mut output = String::new();
        for (row, chunk) in bytes[..shown].chunks(16).enumerate() {
            let _ = write!(output, "{:08x}  ", row * 16);
            for column in 0..16 {
                if let Some(byte) = chunk.get(column) {
                    let _ = write!(output, "{byte:02x} ");
                } else {
                    output.push_str("   ");
                }
                if column == 7 {
                    output.push(' ');
                }
            }
            output.push_str(" |");
            for byte in chunk {
                output.push(if byte.is_ascii_graphic() || *byte == b' ' {
                    char::from(*byte)
                } else {
                    '.'
                });
            }
            output.push('|');
            output.push('\n');
        }
        if total > shown {
            let _ = writeln!(output, "… truncated: showing {shown} of {total} bytes …");
        }
        Self(output)
    }

    /// The ready-to-render selectable text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Marks and imagery.
impl Element {
    /// A bounded, read-only structured JSON document.
    #[must_use]
    pub fn json_view(value: impl Into<String>) -> Self {
        Self::new(Tag::JsonView).value(value)
    }

    /// A bounded list of structured stack frames.
    #[must_use]
    pub fn stack_trace() -> Self {
        Self::new(Tag::StackTrace)
    }

    #[must_use]
    pub fn stack_frame(function: impl Into<String>, location: impl Into<String>) -> Self {
        Self::new(Tag::StackFrame).label(function).value(location)
    }

    /// A safe, selectable Markdown document. HTML is never interpreted.
    #[must_use]
    pub fn markdown_view(value: impl Into<String>) -> Self {
        Self::new(Tag::MarkdownView).value(value)
    }

    /// A run of monospaced text.
    #[must_use]
    pub fn code(value: impl Into<String>) -> Self {
        Self::new(Tag::Code).label(value)
    }

    /// A reference to somewhere else.
    #[must_use]
    pub fn link(label: impl Into<String>, uri: impl Into<String>) -> Self {
        Self::new(Tag::Link).label(label).uri(uri)
    }

    /// A named icon.
    #[must_use]
    pub fn icon_mark(icon: impl Into<String>) -> Self {
        Self::new(Tag::Icon).icon(icon)
    }

    /// A round monogram.
    #[must_use]
    pub fn avatar(initials: impl Into<String>) -> Self {
        Self::new(Tag::Avatar).label(initials)
    }

    /// A row of overlapping monograms.
    #[must_use]
    pub fn avatar_group() -> Self {
        Self::new(Tag::AvatarGroup)
    }

    /// A compact removable token.
    #[must_use]
    pub fn chip(label: impl Into<String>) -> Self {
        Self::new(Tag::Chip).label(label)
    }

    /// A picture, named by a file reference.
    #[must_use]
    pub fn image(uri: impl Into<String>) -> Self {
        Self::new(Tag::Image).uri(uri)
    }

    /// A wrapping grid of pictures.
    #[must_use]
    pub fn image_list() -> Self {
        Self::new(Tag::ImageList)
    }

    /// One picture of an image list.
    #[must_use]
    pub fn image_list_item(uri: impl Into<String>) -> Self {
        Self::new(Tag::ImageListItem).uri(uri)
    }
}

/// Feedback: progress, emptiness and messages.
impl Element {
    /// A bar filled to a fraction of its length.
    #[must_use]
    pub fn progress(fraction: f64) -> Self {
        Self::new(Tag::Progress).prop(Prop::Fraction, PropValue::Number(fraction))
    }

    /// Indeterminate activity.
    #[must_use]
    pub fn spinner() -> Self {
        Self::new(Tag::Spinner)
    }

    /// A measured quantity against its range.
    #[must_use]
    pub fn meter(fraction: f64) -> Self {
        Self::new(Tag::Meter).prop(Prop::Fraction, PropValue::Number(fraction))
    }

    /// The shape of content that has not arrived.
    #[must_use]
    pub fn skeleton() -> Self {
        Self::new(Tag::Skeleton)
    }

    /// What to show where there is nothing to show.
    #[must_use]
    pub fn empty_state(label: impl Into<String>) -> Self {
        Self::new(Tag::EmptyState).label(label)
    }

    /// One measured figure and what it measures.
    #[must_use]
    pub fn stat(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self::new(Tag::Stat).label(label).value(value)
    }

    /// A message that appears and dismisses itself.
    #[must_use]
    pub fn toast(label: impl Into<String>) -> Self {
        Self::new(Tag::Toast).label(label)
    }

    /// A message across the top of a surface.
    #[must_use]
    pub fn banner(label: impl Into<String>) -> Self {
        Self::new(Tag::Banner).label(label)
    }

    /// The heading of a message.
    #[must_use]
    pub fn alert_title(title: impl Into<String>) -> Self {
        Self::new(Tag::AlertTitle).label(title)
    }

    /// A message beside the thing it is about.
    #[must_use]
    pub fn inline_message(label: impl Into<String>) -> Self {
        Self::new(Tag::InlineMessage).label(label)
    }

    /// A bounded group of validation problems and their corrective actions.
    #[must_use]
    pub fn validation_summary(label: impl Into<String>) -> Self {
        Self::new(Tag::ValidationSummary)
            .label(label)
            .icon("dialog-warning-symbolic")
    }
}

/// Long-form content.
impl Element {
    /// A read-only view of source text.
    #[must_use]
    pub fn code_view(value: impl Into<String>) -> Self {
        Self::new(Tag::CodeView).value(value)
    }

    /// A bounded binary view with offset, octet and printable columns.
    #[must_use]
    pub fn hex_view(source: HexSource<'_>) -> Self {
        Self::new(Tag::HexView).value(HexView::new(source).0)
    }

    /// An append-only view that follows its tail.
    ///
    /// Every value sent to it is appended, so a producer streams lines rather
    /// than resending the whole log.
    #[must_use]
    pub fn log_view() -> Self {
        Self::new(Tag::LogView)
    }

    /// A bounded collection of unified lines or side-by-side diff regions.
    #[must_use]
    pub fn diff_viewer() -> Self {
        Self::new(Tag::DiffViewer)
    }

    /// One independently readable diff line: status in `label`, content in `value`.
    #[must_use]
    pub fn diff_line(status: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(Tag::DiffLine).label(status).value(content)
    }

    /// A compact trend over at most 64 finite samples.
    #[must_use]
    pub fn sparkline(samples: impl IntoIterator<Item = f64>) -> Self {
        let value = samples
            .into_iter()
            .filter(|sample| sample.is_finite())
            .take(crate::SPARKLINE_SAMPLE_LIMIT)
            .map(|sample| sample.to_string())
            .collect::<Vec<_>>()
            .join(",");
        Self::new(Tag::Sparkline).value(value)
    }

    /// A bounded sampled profile rendered as labelled proportional bars.
    #[must_use]
    pub fn flame_graph(frames: impl IntoIterator<Item = FlameFrame>) -> Self {
        let value = frames
            .into_iter()
            .take(crate::FLAME_GRAPH_FRAME_LIMIT)
            .map(|frame| format!("{}\t{}", frame.samples, frame.label))
            .collect::<Vec<_>>()
            .join("\n");
        Self::new(Tag::FlameGraph).value(value)
    }

    /// A bounded process address map with exact ranges and permissions.
    #[must_use]
    pub fn memory_map(regions: impl IntoIterator<Item = MemoryRegion>) -> Self {
        let value = regions
            .into_iter()
            .take(crate::MEMORY_MAP_REGION_LIMIT)
            .map(|region| {
                format!(
                    "{:016x}-{:016x}\t{}\t{}\t{}",
                    region.start,
                    region.end,
                    region.permissions,
                    region.end - region.start,
                    region.mapping
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Self::new(Tag::MemoryMap).value(value)
    }

    /// A bounded decoded instruction listing with exact source bytes.
    #[must_use]
    pub fn disassembly_view(instructions: impl IntoIterator<Item = Instruction>) -> Self {
        let value = instructions
            .into_iter()
            .take(crate::DISASSEMBLY_INSTRUCTION_LIMIT)
            .map(|instruction| {
                let bytes = instruction
                    .bytes
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!(
                    "{:016x}\t{}\t{}\t{}",
                    instruction.address, bytes, instruction.mnemonic, instruction.operands
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Self::new(Tag::DisassemblyView).value(value)
    }

    /// A bounded chronological event view.
    #[must_use]
    pub fn timeline_view(events: impl IntoIterator<Item = TimelineEvent>) -> Self {
        let value = events
            .into_iter()
            .take(crate::TIMELINE_EVENT_LIMIT)
            .map(|event| {
                format!(
                    "{}\t{}\t{}\t{}",
                    event.timestamp_ms, event.category, event.label, event.detail
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Self::new(Tag::TimelineView).value(value)
    }

    #[must_use]
    pub fn test_report_view(cases: impl IntoIterator<Item = TestCase>) -> Self {
        let value = cases
            .into_iter()
            .take(crate::TEST_REPORT_CASE_LIMIT)
            .map(|case| {
                format!(
                    "{}\t{}\t{}\t{}\t{}",
                    case.suite,
                    case.name,
                    case.status.as_str(),
                    case.duration_ms,
                    case.failure
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Self::new(Tag::TestReportView).value(value)
    }
    #[must_use]
    pub fn coverage_view(source: CoverageSource<'_>) -> Self {
        Self::new(Tag::CoverageView).value(CoverageView::new(source).0)
    }

    #[must_use]
    pub fn network_waterfall(source: NetworkSource<'_>) -> Self {
        let (requests, total) = match source {
            NetworkSource::Exact(v) => (v, v.len()),
            NetworkSource::Bounded { prefix, total_requests } => (prefix, total_requests.max(prefix.len())),
        };
        let shown = requests.len().min(crate::NETWORK_WATERFALL_REQUEST_LIMIT);
        let detail = if total > shown {
            format!("truncated: showing {shown} of {total} requests")
        } else {
            format!("showing all {shown} requests")
        };
        Self::new(Tag::NetworkWaterfall)
            .label(format!("{shown} requests"))
            .detail(detail)
            .children(requests[..shown].iter().enumerate().map(|(index, request)| {
                let status = request.status.map_or_else(|| "pending".to_owned(), |v| v.to_string());
                Self::new(Tag::NetworkRequest)
                    .key(index.to_string())
                    .label(format!("{} {}", request.method.as_str(), request.url))
                    .value(format!(
                        "start_us={} duration_us={} status={} bytes={} detail={}",
                        request.start_us, request.duration_us, status, request.bytes, request.detail
                    ))
                    .children(request.phases.iter().map(|phase| {
                        Self::new(Tag::NetworkPhase).label(phase.kind.as_str()).value(format!(
                            "offset_us={} duration_us={} total_us={}",
                            phase.offset_us, phase.duration_us, request.duration_us
                        ))
                    }))
            }))
    }
    #[must_use]
    pub fn dependency_graph(source: DependencySource<'_>) -> Option<Self> {
        let (nodes, tn, edges, te, cycles, tc, bounded) = match source {
            DependencySource::Exact { nodes, edges, cycles } => {
                (nodes, nodes.len(), edges, edges.len(), cycles, cycles.len(), false)
            }
            DependencySource::Bounded {
                nodes,
                total_nodes,
                edges,
                total_edges,
                cycles,
                total_cycles,
            } => (nodes, total_nodes, edges, total_edges, cycles, total_cycles, true),
        };
        if nodes.len() > crate::DEPENDENCY_GRAPH_NODE_LIMIT
            || edges.len() > crate::DEPENDENCY_GRAPH_EDGE_LIMIT
            || cycles.len() > crate::DEPENDENCY_GRAPH_CYCLE_LIMIT
            || tn < nodes.len()
            || te < edges.len()
            || tc < cycles.len()
        {
            return None;
        }
        let ids: std::collections::BTreeSet<_> = nodes.iter().map(|n| n.id.as_str()).collect();
        if ids.len() != nodes.len()
            || edges
                .iter()
                .any(|e| !ids.contains(e.source.as_str()) || !ids.contains(e.target.as_str()))
        {
            return None;
        }
        let edge_ids: std::collections::BTreeSet<_> = edges
            .iter()
            .map(|e| (e.source.as_str(), e.target.as_str(), e.relation.as_str()))
            .collect();
        if edge_ids.len() != edges.len() {
            return None;
        }
        for c in cycles {
            if c.members.iter().any(|m| !ids.contains(m.as_str())) {
                return None;
            }
            for i in 0..c.members.len() {
                let a = &c.members[i];
                let b = &c.members[(i + 1) % c.members.len()];
                if !edges.iter().any(|e| e.source == *a && e.target == *b) {
                    return None;
                }
            }
        }
        let detail = if bounded {
            format!(
                "bounded source: nodes {}/{tn}, edges {}/{te}, cycles {}/{tc}",
                nodes.len(),
                edges.len(),
                cycles.len()
            )
        } else {
            "complete dependency graph".into()
        };
        let mut children = nodes
            .iter()
            .map(|n| {
                Self::new(Tag::DependencyNode)
                    .key(&n.id)
                    .label(format!("{}@{}", n.label, n.version))
                    .value(format!("id={} state={} detail={}", n.id, n.state.as_str(), n.detail))
                    .children(edges.iter().filter(|e| e.source == n.id).map(|e| {
                        Self::new(Tag::DependencyEdge)
                            .label(format!("{} → {}", e.relation.as_str(), e.target))
                            .value(format!("requirement={}", e.requirement))
                    }))
            })
            .collect::<Vec<_>>();
        children.extend(cycles.iter().enumerate().map(|(i, c)| {
            Self::new(Tag::DependencyCycle)
                .label(format!("cycle {}", i + 1))
                .detail(format!("{} members", c.members.len()))
                .children(c.members.iter().enumerate().map(|(j, m)| {
                    Self::new(Tag::DependencyCycleMember)
                        .key(j.to_string())
                        .label(m)
                        .value(format!("position={j}"))
                }))
        }));
        Some(
            Self::new(Tag::DependencyGraph)
                .label(format!("{} dependencies", nodes.len()))
                .detail(detail)
                .children(children),
        )
    }
    #[must_use]
    pub fn query_plan(source: QueryPlanSource<'_>) -> Option<Self> {
        fn count(nodes: &[QueryPlanNode], depth: usize, ids: &mut std::collections::BTreeSet<String>) -> Option<usize> {
            let mut n = 0;
            for node in nodes {
                if depth > crate::QUERY_PLAN_DEPTH_LIMIT {
                    return None;
                }
                if !ids.insert(node.id.clone()) {
                    return None;
                }
                n += 1 + count(&node.children, depth + 1, ids)?
            }
            Some(n)
        }
        fn element(node: &QueryPlanNode) -> Element {
            Element::new(Tag::QueryPlanNode)
                .key(&node.id)
                .label(format!("{} · {}", node.operator.as_str(), node.label))
                .value(format!(
                    "id={} operator={} state={} relation={} detail={}",
                    node.id,
                    node.operator.as_str(),
                    node.state.as_str(),
                    node.relation,
                    node.detail
                ))
                .children(node.metrics.iter().map(|m| {
                    Element::new(Tag::QueryPlanMetric)
                        .label(m.kind.as_str())
                        .value(m.value.to_string())
                }))
                .children(node.children.iter().map(element))
        }
        let (roots, total, bounded) = match source {
            QueryPlanSource::Exact(v) => (v, 0, false),
            QueryPlanSource::Bounded { prefix, total_nodes } => (prefix, total_nodes, true),
        };
        let shown = count(roots, 1, &mut std::collections::BTreeSet::new())?;
        let total = if bounded {
            if total < shown {
                return None;
            }
            total
        } else {
            shown
        };
        if shown > crate::QUERY_PLAN_NODE_LIMIT {
            return None;
        }
        let detail = if bounded {
            format!("bounded source: showing {shown} of {total} operators")
        } else {
            "complete query plan".into()
        };
        Some(
            Self::new(Tag::QueryPlan)
                .label(format!("{shown} plan operators"))
                .detail(detail)
                .children(roots.iter().map(element)),
        )
    }

    /// A playable file.
    #[must_use]
    pub fn video(uri: impl Into<String>) -> Self {
        Self::new(Tag::Video).uri(uri)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CoverageLine, CoverageSource, CoverageView, DependencyCycle, DependencyEdge, DependencyNode,
        DependencyRelation, DependencySource, DependencyState, FlameFrame, HexSource, HexView, HttpMethod, Instruction,
        MemoryRegion, NetworkPhase, NetworkPhaseKind, NetworkRequest, NetworkSource, QueryMetric, QueryMetricKind,
        QueryNodeState, QueryOperator, QueryPlanNode, QueryPlanSource, TestCase, TestStatus, TimelineEvent,
    };
    use crate::{Element, HEX_VIEW_BYTE_LIMIT};

    #[test]
    fn hex_rows_are_fixed_width_and_printable() {
        let view = HexView::new(HexSource::Exact(b"A\0z0123456789abcdef"));
        let lines: Vec<_> = view.as_str().lines().collect();
        assert_eq!(
            lines[0],
            "00000000  41 00 7a 30 31 32 33 34  35 36 37 38 39 61 62 63  |A.z0123456789abc|"
        );
        assert_eq!(
            lines[1],
            "00000010  64 65 66                                          |def|"
        );
    }

    #[test]
    fn bounded_source_discloses_omitted_bytes() {
        let bytes = vec![0xff; HEX_VIEW_BYTE_LIMIT + 8];
        let view = HexView::new(HexSource::Bounded {
            prefix: &bytes,
            total_bytes: 9_000,
        });
        assert!(view.as_str().ends_with("… truncated: showing 4096 of 9000 bytes …\n"));
        assert_eq!(
            view.as_str()
                .lines()
                .filter(|line| line.starts_with(|c: char| c.is_ascii_digit()))
                .count(),
            256
        );
    }

    #[test]
    fn sparkline_keeps_only_finite_bounded_samples() {
        let element = Element::sparkline((0..100).map(|value| if value == 2 { f64::NAN } else { f64::from(value) }));
        let mut reconciliation = crate::Reconciliation::new();
        let frame = reconciliation.reconcile(&element);
        let value = frame
            .patches
            .iter()
            .find_map(|patch| match patch {
                crate::Patch::SetProp {
                    prop: crate::Prop::Value,
                    value,
                    ..
                } => value.as_text(),
                _ => None,
            })
            .expect("sparkline value");
        assert_eq!(value.split(',').count(), crate::SPARKLINE_SAMPLE_LIMIT);
        assert!(!value.contains("NaN"));
    }

    #[test]
    fn flame_graph_rejects_empty_frames_and_caps_the_wire_value() {
        assert!(FlameFrame::new("idle", 0).is_none());
        assert!(FlameFrame::new(" ", 1).is_none());
        let frames = (0..100).filter_map(|index| FlameFrame::new(format!("worker\t{index}"), index + 1));
        let element = Element::flame_graph(frames);
        let mut reconciliation = crate::Reconciliation::new();
        let frame = reconciliation.reconcile(&element);
        let value = frame
            .patches
            .iter()
            .find_map(|patch| match patch {
                crate::Patch::SetProp {
                    prop: crate::Prop::Value,
                    value,
                    ..
                } => value.as_text(),
                _ => None,
            })
            .expect("flame graph value");
        assert_eq!(
            value.lines().count(),
            64,
            "the public ceiling is part of the component contract"
        );
        assert!(!value.contains('\t') || value.lines().all(|line| line.matches('\t').count() == 1));
        assert!(value.contains("worker 0"));
    }

    #[test]
    fn memory_map_rejects_invalid_regions_and_has_an_independent_ceiling() {
        assert!(MemoryRegion::new(9, 9, "rw-p", "heap").is_none());
        assert!(MemoryRegion::new(1, 2, "danger", "bad").is_none());
        let regions = (0..200).filter_map(|index| {
            let start = index * 4096;
            MemoryRegion::new(start, start + 4096, "r-xp", format!("segment\t{index}"))
        });
        let element = Element::memory_map(regions);
        let mut reconciliation = crate::Reconciliation::new();
        let frame = reconciliation.reconcile(&element);
        let value = frame
            .patches
            .iter()
            .find_map(|patch| match patch {
                crate::Patch::SetProp {
                    prop: crate::Prop::Value,
                    value,
                    ..
                } => value.as_text(),
                _ => None,
            })
            .expect("memory map value");
        assert_eq!(
            value.lines().count(),
            128,
            "the public region ceiling is a fixed contract"
        );
        assert!(value.starts_with("0000000000000000-0000000000001000\tr-xp\t4096\tsegment 0"));
    }

    #[test]
    fn disassembly_rejects_invalid_encodings_and_has_an_independent_ceiling() {
        assert!(Instruction::new(0, [], "ret", "").is_none());
        assert!(Instruction::new(0, [0xc3], " ", "").is_none());
        let instructions = (0..300).filter_map(|index| Instruction::new(index, [0x48, 0x89, 0xe5], "mov", "rbp\t rsp"));
        let element = Element::disassembly_view(instructions);
        let mut reconciliation = crate::Reconciliation::new();
        let frame = reconciliation.reconcile(&element);
        let value = frame
            .patches
            .iter()
            .find_map(|patch| match patch {
                crate::Patch::SetProp {
                    prop: crate::Prop::Value,
                    value,
                    ..
                } => value.as_text(),
                _ => None,
            })
            .expect("disassembly value");
        assert_eq!(
            value.lines().count(),
            256,
            "the instruction ceiling is a fixed contract"
        );
        assert!(value.starts_with("0000000000000000\t48 89 e5\tmov\trbp  rsp"));
    }

    #[test]
    fn timeline_rejects_blank_events_and_has_an_independent_ceiling() {
        assert!(TimelineEvent::new(0, "runtime", " ", "ignored").is_none());
        let events =
            (0..300).filter_map(|index| TimelineEvent::new(index, "runtime", format!("event\t{index}"), "detail"));
        let element = Element::timeline_view(events);
        let mut reconciliation = crate::Reconciliation::new();
        let frame = reconciliation.reconcile(&element);
        let value = frame
            .patches
            .iter()
            .find_map(|patch| match patch {
                crate::Patch::SetProp {
                    prop: crate::Prop::Value,
                    value,
                    ..
                } => value.as_text(),
                _ => None,
            })
            .expect("timeline value");
        assert_eq!(value.lines().count(), 256, "the event ceiling is a fixed contract");
        assert!(value.starts_with("0\truntime\tevent 0\tdetail"));
    }

    #[test]
    fn test_report_bounds_cases_and_failure_detail_independently() {
        assert!(TestCase::new("", "works", TestStatus::Passed, 1, "").is_none());
        let cases = (0..300).filter_map(|index| {
            TestCase::new(
                "api",
                format!("case\t{index}"),
                TestStatus::Failed,
                index,
                "x".repeat(600),
            )
        });
        let element = Element::test_report_view(cases);
        let mut reconciliation = crate::Reconciliation::new();
        let frame = reconciliation.reconcile(&element);
        let value = frame
            .patches
            .iter()
            .find_map(|patch| match patch {
                crate::Patch::SetProp {
                    prop: crate::Prop::Value,
                    value,
                    ..
                } => value.as_text(),
                _ => None,
            })
            .expect("report value");
        assert_eq!(value.lines().count(), 256, "the case ceiling is fixed");
        assert_eq!(
            value
                .lines()
                .next()
                .unwrap()
                .split('\t')
                .nth(4)
                .unwrap()
                .chars()
                .count(),
            512,
            "failure detail is independently bounded"
        );
        assert!(value.starts_with("api\tcase 0\tfailed\t0\t"));
    }

    #[test]
    fn coverage_has_fixed_row_text_bounds_and_visible_truncation() {
        assert!(CoverageLine::new(0, 1, "invalid").is_none());
        let lines = (1..=600)
            .filter_map(|line| CoverageLine::new(line, u64::from(line % 2), format!("source\t{}", "x".repeat(600))))
            .collect::<Vec<_>>();
        let view = CoverageView::new(CoverageSource::Bounded {
            prefix: &lines,
            total_lines: 900,
        });
        assert_eq!(view.as_str().lines().count(), 513, "512 rows plus truncation marker");
        assert!(view.as_str().ends_with("… showing 512 of 900 lines …"));
        assert_eq!(
            view.as_str()
                .lines()
                .next()
                .unwrap()
                .split('\t')
                .nth(2)
                .unwrap()
                .chars()
                .count(),
            512
        );
    }

    #[test]
    fn network_waterfall_rejects_invalid_timing_and_bounds_exact_semantics() {
        assert!(NetworkPhase::new(NetworkPhaseKind::Dns, 0, 0).is_none());
        let overlapping = [
            NetworkPhase::new(NetworkPhaseKind::Dns, 0, 5).unwrap(),
            NetworkPhase::new(NetworkPhaseKind::Connect, 4, 2).unwrap(),
        ];
        assert!(NetworkRequest::new(HttpMethod::Get, "https://bad", 0, 10, Some(200), 1, "", overlapping).is_none());
        let phase = NetworkPhase::new(NetworkPhaseKind::Wait, 2, 3).unwrap();
        let requests = (0..40)
            .filter_map(|index| {
                NetworkRequest::new(
                    HttpMethod::Get,
                    format!("https://example.test/{index}\n{}", "x".repeat(200)),
                    0,
                    10,
                    Some(200),
                    42,
                    "detail\tclean",
                    [phase.clone()],
                )
            })
            .collect::<Vec<_>>();
        let element = Element::network_waterfall(NetworkSource::Bounded {
            prefix: &requests,
            total_requests: 99,
        });
        let mut reconciliation = crate::Reconciliation::new();
        let frame = reconciliation.reconcile(&element);
        assert_eq!(
            frame
                .patches
                .iter()
                .filter(|p| matches!(
                    p,
                    crate::Patch::Create {
                        tag: crate::Tag::NetworkRequest,
                        ..
                    }
                ))
                .count(),
            crate::NETWORK_WATERFALL_REQUEST_LIMIT
        );
        assert_eq!(
            frame
                .patches
                .iter()
                .filter(|p| matches!(
                    p,
                    crate::Patch::Create {
                        tag: crate::Tag::NetworkPhase,
                        ..
                    }
                ))
                .count(),
            crate::NETWORK_WATERFALL_REQUEST_LIMIT
        );
        assert!(frame.patches.iter().any(|p| matches!(p, crate::Patch::SetProp { prop: crate::Prop::Detail, value, .. } if value.as_text() == Some("truncated: showing 32 of 99 requests"))));
        assert!(
            !frame
                .patches
                .iter()
                .filter_map(|p| if let crate::Patch::SetProp { value, .. } = p {
                    value.as_text()
                } else {
                    None
                })
                .any(|v| v.contains('\n') || v.contains('\t'))
        );
    }

    #[test]
    fn dependency_graph_is_referential_bounded_and_cycle_exact() {
        let nodes = (0..32)
            .map(|i| {
                DependencyNode::new(
                    format!("n{i}"),
                    format!("node{i}"),
                    "1.0",
                    if i == 1 {
                        DependencyState::Conflict
                    } else {
                        DependencyState::Resolved
                    },
                    "detail\n",
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let mut edges = (0..31)
            .map(|i| {
                DependencyEdge::new(
                    format!("n{i}"),
                    format!("n{}", i + 1),
                    DependencyRelation::Runtime,
                    "^1",
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        edges.push(DependencyEdge::new("n31", "n0", DependencyRelation::Runtime, "^1").unwrap());
        let cycle = DependencyCycle::new(["n0", "n1", "n2"]).unwrap();
        assert!(
            Element::dependency_graph(DependencySource::Exact {
                nodes: &nodes,
                edges: &edges,
                cycles: &[cycle]
            })
            .is_none(),
            "cycle closure must be an actual edge"
        );
        assert!(
            Element::dependency_graph(DependencySource::Exact {
                nodes: &nodes,
                edges: &[DependencyEdge::new("n0", "absent", DependencyRelation::Build, "*").unwrap()],
                cycles: &[]
            })
            .is_none()
        );
        let graph = Element::dependency_graph(DependencySource::Bounded {
            nodes: &nodes,
            total_nodes: 90,
            edges: &edges,
            total_edges: 500,
            cycles: &[],
            total_cycles: 0,
        })
        .unwrap();
        let mut r = crate::Reconciliation::new();
        let f = r.reconcile(&graph);
        assert_eq!(
            f.patches
                .iter()
                .filter(|p| matches!(
                    p,
                    crate::Patch::Create {
                        tag: crate::Tag::DependencyNode,
                        ..
                    }
                ))
                .count(),
            32
        );
        assert!(f.patches.iter().any(|p|matches!(p,crate::Patch::SetProp{prop:crate::Prop::Detail,value,..} if value.as_text()==Some("bounded source: nodes 32/90, edges 32/500, cycles 0/0"))));
    }
    fn query_metrics() -> Vec<QueryMetric> {
        [QueryMetricKind::EstimatedRows, QueryMetricKind::ActualRows, QueryMetricKind::Cost,
         QueryMetricKind::DurationUs, QueryMetricKind::Loops]
            .into_iter().enumerate()
            .map(|(index, kind)| QueryMetric::new(kind, index as f64 + 1.0).unwrap()).collect()
    }
    fn query_node(id: impl Into<String>, children: impl IntoIterator<Item = QueryPlanNode>) -> QueryPlanNode {
        QueryPlanNode::new(id, QueryOperator::TableScan, "users", "users", QueryNodeState::Hot,
            "slow", query_metrics(), children).unwrap()
    }
    #[test]
    fn query_plan_maximum_is_exactly_217_semantic_nodes_without_truncation() {
        let nodes = (0..crate::QUERY_PLAN_NODE_LIMIT).map(|i| query_node(format!("n{i}"), [])).collect::<Vec<_>>();
        let plan = Element::query_plan(QueryPlanSource::Exact(&nodes)).unwrap();
        let frame = crate::Reconciliation::new().reconcile(&plan);
        assert_eq!(frame.patches.iter().filter(|p| matches!(p, crate::Patch::Create { .. })).count(),
            1 + crate::QUERY_PLAN_NODE_LIMIT * (1 + crate::QUERY_PLAN_METRIC_LIMIT));
        assert!(frame.patches.iter().any(|p| matches!(p, crate::Patch::SetProp { prop: crate::Prop::Detail, value, .. }
            if value.as_text() == Some("complete query plan"))));
    }
    #[test]
    fn query_plan_rejects_the_37th_operator() {
        let nodes = (0..=crate::QUERY_PLAN_NODE_LIMIT).map(|i| query_node(format!("n{i}"), [])).collect::<Vec<_>>();
        assert!(Element::query_plan(QueryPlanSource::Exact(&nodes)).is_none());
    }
    #[test]
    fn query_plan_accepts_twelve_levels_and_rejects_thirteen() {
        fn chain(depth: usize) -> QueryPlanNode {
            let mut node = query_node(format!("n{}", depth - 1), []);
            for level in (0..depth - 1).rev() { node = query_node(format!("n{level}"), [node]); }
            node
        }
        assert!(Element::query_plan(QueryPlanSource::Exact(&[chain(crate::QUERY_PLAN_DEPTH_LIMIT)])).is_some());
        assert!(Element::query_plan(QueryPlanSource::Exact(&[chain(crate::QUERY_PLAN_DEPTH_LIMIT + 1)])).is_none());
    }
    #[test]
    fn query_plan_rejects_duplicate_metric_kinds() {
        let duration = QueryMetric::new(QueryMetricKind::DurationUs, 42.0).unwrap();
        assert!(QueryPlanNode::new("scan", QueryOperator::TableScan, "users", "users", QueryNodeState::Normal,
            "", [duration.clone(), duration], []).is_none());
    }
    #[test]
    fn query_plan_detail_is_control_safe_and_bounded() {
        let node = QueryPlanNode::new("scan", QueryOperator::TableScan, "users", "users", QueryNodeState::Normal,
            format!("{}\nignored", "x".repeat(200)), [], []).unwrap();
        assert_eq!(node.detail.chars().count(), 160);
        assert!(!node.detail.chars().any(char::is_control));
    }
    #[test]
    fn query_plan_bounded_source_reports_authoritative_provenance() {
        let leaf = query_node("scan", []);
        let plan = Element::query_plan(QueryPlanSource::Bounded { prefix: &[leaf], total_nodes: 90 }).unwrap();
        let frame = crate::Reconciliation::new().reconcile(&plan);
        assert!(frame.patches.iter().any(|p| matches!(p, crate::Patch::SetProp { prop: crate::Prop::Detail, value, .. }
            if value.as_text() == Some("bounded source: showing 1 of 90 operators"))));
        assert!(QueryMetric::new(QueryMetricKind::Loops, f64::NAN).is_none());
    }
}
