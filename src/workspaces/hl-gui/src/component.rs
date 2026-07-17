use crate::Settings;

/// Stable application-owned identity for a user interaction.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct EventId(String);

impl EventId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// User intent emitted by components. Meaning and side effects belong to the composing application.
#[derive(Clone, PartialEq, Debug)]
pub enum Event {
    Invoke(EventId),
    Change { id: EventId, value: crate::Value },
    Submit(EventId),
}

pub trait Events {
    fn emit(&mut self, event: Event);
}

/// A reusable component contributes a toolkit-neutral element tree.
pub trait Component {
    fn view(&self) -> Element;
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct View {
    pub title: String,
    pub content: Vec<Element>,
}

impl View {
    pub fn new(title: impl Into<String>, content: impl IntoIterator<Item = Element>) -> Self {
        Self {
            title: title.into(),
            content: content.into_iter().collect(),
        }
    }
}

/// One application-supplied row in a generic selectable list.
#[derive(Clone, PartialEq, Debug)]
pub struct ListItem {
    pub title: String,
    pub subtitle: Option<String>,
    pub event: EventId,
    pub enabled: bool,
    pub selected: bool,
}

impl ListItem {
    pub fn new(title: impl Into<String>, event: EventId) -> Self {
        Self {
            title: title.into(),
            subtitle: None,
            event,
            enabled: true,
            selected: false,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Element {
    Column(Vec<Element>),
    Row(Vec<Element>),
    Text(String),
    Button {
        label: String,
        event: EventId,
        enabled: bool,
    },
    Section {
        title: Option<String>,
        content: Vec<Element>,
    },
    List(Vec<ListItem>),
    Settings(Settings),
    Spacer,
}

impl Element {
    pub fn column(children: impl IntoIterator<Item = Element>) -> Self {
        Self::Column(children.into_iter().collect())
    }

    pub fn row(children: impl IntoIterator<Item = Element>) -> Self {
        Self::Row(children.into_iter().collect())
    }

    pub fn button(label: impl Into<String>, event: EventId) -> Self {
        Self::Button {
            label: label.into(),
            event,
            enabled: true,
        }
    }

    pub fn section(title: impl Into<String>, content: impl IntoIterator<Item = Element>) -> Self {
        Self::Section {
            title: Some(title.into()),
            content: content.into_iter().collect(),
        }
    }

    pub fn list(items: impl IntoIterator<Item = ListItem>) -> Self {
        Self::List(items.into_iter().collect())
    }
}
