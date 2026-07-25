use crate::Container;

/// Selection applied when inactive containers are pruned.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Prune {
    before_ms: Option<u64>,
    labels: Vec<Label>,
    excluded_labels: Vec<Label>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Label {
    name: String,
    value: Option<String>,
}

impl Prune {
    /// Selects containers created strictly before the timestamp.
    #[must_use]
    pub const fn before(mut self, timestamp_ms: u64) -> Self {
        self.before_ms = Some(timestamp_ms);
        self
    }

    /// Requires a label name, optionally with an exact value after `=`.
    #[must_use]
    pub fn label(mut self, value: impl AsRef<str>) -> Self {
        self.labels.push(Label::parse(value.as_ref()));
        self
    }

    /// Excludes containers carrying a label name or exact label value.
    #[must_use]
    pub fn without_label(mut self, value: impl AsRef<str>) -> Self {
        self.excluded_labels.push(Label::parse(value.as_ref()));
        self
    }

    pub(crate) fn matches(&self, container: &Container) -> bool {
        self.before_ms
            .is_none_or(|timestamp| container.created_at_ms < timestamp)
            && self.labels.iter().all(|label| label.matches(container))
            && self
                .excluded_labels
                .iter()
                .all(|label| !label.matches(container))
    }
}

impl Label {
    fn parse(value: &str) -> Self {
        value.split_once('=').map_or_else(
            || Self {
                name: value.to_owned(),
                value: None,
            },
            |(name, value)| Self {
                name: name.to_owned(),
                value: Some(value.to_owned()),
            },
        )
    }

    fn matches(&self, container: &Container) -> bool {
        self.value.as_ref().map_or_else(
            || container.spec.labels.contains_key(&self.name),
            |value| container.spec.labels.get(&self.name) == Some(value),
        )
    }
}
