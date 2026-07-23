use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Object identity and searchable attributes carried by a Docker event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Actor {
    #[serde(rename = "ID")]
    pub id: String,
    pub attributes: BTreeMap<String, String>,
}

/// One Docker-compatible lifecycle event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Event {
    #[serde(rename = "Type")]
    pub kind: String,
    pub action: String,
    pub actor: Actor,
    #[serde(rename = "scope")]
    pub scope: String,
    #[serde(rename = "time")]
    pub time: i64,
    #[serde(rename = "timeNano")]
    pub time_nano: i64,
    #[serde(rename = "status", skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(rename = "id", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "from", skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// Typed conjunction of Docker event filters. Values within one key are alternatives.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EventFilter(pub BTreeMap<String, Vec<String>>);

impl EventFilter {
    #[must_use]
    pub fn kind(self, value: impl Into<String>) -> Self {
        self.value("type", value)
    }

    #[must_use]
    pub fn action(self, value: impl Into<String>) -> Self {
        self.value("event", value)
    }

    #[must_use]
    pub fn container(self, value: impl Into<String>) -> Self {
        self.value("container", value)
    }

    #[must_use]
    pub fn image(self, value: impl Into<String>) -> Self {
        self.value("image", value)
    }

    #[must_use]
    pub fn network(self, value: impl Into<String>) -> Self {
        self.value("network", value)
    }

    #[must_use]
    pub fn volume(self, value: impl Into<String>) -> Self {
        self.value("volume", value)
    }

    #[must_use]
    pub fn label(self, value: impl Into<String>) -> Self {
        self.value("label", value)
    }

    fn value(mut self, key: &str, value: impl Into<String>) -> Self {
        self.0.entry(key.into()).or_default().push(value.into());
        self
    }
}

/// Replay and termination bounds for an event subscription, in Unix seconds.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EventQuery {
    pub filters: EventFilter,
    pub since: Option<i64>,
    pub until: Option<i64>,
}

impl EventQuery {
    #[must_use]
    pub fn filters(mut self, value: EventFilter) -> Self {
        self.filters = value;
        self
    }

    #[must_use]
    pub const fn since(mut self, value: i64) -> Self {
        self.since = Some(value);
        self
    }

    #[must_use]
    pub const fn until(mut self, value: i64) -> Self {
        self.until = Some(value);
        self
    }
}

impl Event {
    /// Serialize this event as one newline-delimited Docker event record.
    ///
    /// # Errors
    /// Returns a JSON serialization error if the wire model cannot be encoded.
    pub fn line(&self) -> Result<Vec<u8>, serde_json::Error> {
        let mut line = serde_json::to_vec(self)?;
        line.push(b'\n');
        Ok(line)
    }
}

#[cfg(test)]
mod tests {
    use super::{Actor, Event};

    #[test]
    fn docker_event_keys_and_legacy_fields_match_the_wire_contract() {
        let event = Event {
            kind: "container".into(),
            action: "create".into(),
            actor: Actor {
                id: "abc".into(),
                attributes: std::collections::BTreeMap::default(),
            },
            scope: "local".into(),
            time: 1,
            time_nano: 2,
            status: Some("create".into()),
            id: Some("abc".into()),
            from: Some("alpine".into()),
        };
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["Type"], "container");
        assert_eq!(value["Action"], "create");
        assert_eq!(value["Actor"]["ID"], "abc");
        assert_eq!(value["scope"], "local");
        assert_eq!(value["timeNano"], 2);
        assert_eq!(value["status"], "create");
        assert_eq!(value["id"], "abc");
        assert_eq!(value["from"], "alpine");
    }
}
