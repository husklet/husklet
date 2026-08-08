use crate::EventId;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct FieldId(String);

impl FieldId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Value {
    Text(String),
    Boolean(bool),
    Choice(String),
    Number(i64),
    Empty,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FieldKind {
    Text { placeholder: String, secret: bool },
    Toggle,
    Choices(Vec<Choice>),
    Number { minimum: Option<i64>, maximum: Option<i64> },
}

#[derive(Clone, PartialEq, Debug)]
pub struct Field {
    pub id: FieldId,
    pub label: String,
    pub help: Option<String>,
    pub kind: FieldKind,
    pub value: Value,
    pub change: EventId,
    pub enabled: bool,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Settings {
    pub title: Option<String>,
    pub fields: Vec<Field>,
    pub submit: Option<EventId>,
}

impl Settings {
    pub fn new(fields: impl IntoIterator<Item = Field>) -> Self {
        Self {
            fields: fields.into_iter().collect(),
            ..Self::default()
        }
    }
}
