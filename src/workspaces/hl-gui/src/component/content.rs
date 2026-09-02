//! Constructors for the components that show something: marks, imagery,
//! feedback and long-form content.

use crate::element::Element;
use crate::node::{Prop, PropValue, Tag};

/// Marks and imagery.
impl Element {
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
        Self::new(Tag::ValidationSummary).label(label).icon("dialog-warning-symbolic")
    }
}

/// Long-form content.
impl Element {
    /// A read-only view of source text.
    #[must_use]
    pub fn code_view(value: impl Into<String>) -> Self {
        Self::new(Tag::CodeView).value(value)
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

    /// A playable file.
    #[must_use]
    pub fn video(uri: impl Into<String>) -> Self {
        Self::new(Tag::Video).uri(uri)
    }
}
