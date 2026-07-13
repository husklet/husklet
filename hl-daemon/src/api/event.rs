//! `/events` DTOs.

use serde::Serialize;
use serde_json::Value;

// ---- events ----------------------------------------------------------------

/// A Docker `events` lifecycle document. `Type`/`Action`/`Actor` are the modern fields; `scope`/
/// `time`/`timeNano` are lowercase, and `status`/`id`/`from` are the legacy top-level aliases emitted
/// only for `container` events (hence `Option` + skip-if-none).
#[derive(Serialize)]
pub(crate) struct Event {
    #[serde(rename = "Type")]
    pub type_: String,
    #[serde(rename = "Action")]
    pub action: String,
    #[serde(rename = "Actor")]
    pub actor: Actor,
    pub scope: &'static str,
    pub time: i64,
    #[serde(rename = "timeNano")]
    pub time_nano: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct Actor {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "Attributes")]
    pub attributes: Value,
}
