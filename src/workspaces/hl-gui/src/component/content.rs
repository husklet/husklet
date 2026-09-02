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

    /// A playable file.
    #[must_use]
    pub fn video(uri: impl Into<String>) -> Self {
        Self::new(Tag::Video).uri(uri)
    }
}

#[cfg(test)]
mod tests {
    use super::{FlameFrame, HexSource, HexView, Instruction, MemoryRegion, TimelineEvent};
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
}
