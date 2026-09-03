//! Canonical normalized wire representation of toolkit interactions.

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "interaction", deny_unknown_fields)]
pub enum UiEvent {
    #[serde(rename = "invoke")]
    Invoke {
        trigger: String,
        node: u64,
        id: String,
        slot: Option<String>,
    },
    #[serde(rename = "submit")]
    Submit {
        trigger: String,
        node: u64,
        id: String,
        slot: Option<String>,
    },
    #[serde(rename = "change")]
    Change {
        trigger: String,
        node: u64,
        id: String,
        slot: Option<String>,
        value: hl_gui::PropValue,
    },
    #[serde(rename = "select")]
    Select {
        trigger: String,
        node: u64,
        id: String,
        slot: Option<String>,
        rows: Vec<u64>,
        collection: Option<UiCollectionSelection>,
    },
    #[serde(rename = "scroll")]
    Scroll {
        trigger: String,
        node: u64,
        id: String,
        slot: Option<String>,
        dx: f64,
        dy: f64,
    },
    #[serde(rename = "close")]
    Close {
        trigger: String,
        node: u64,
        id: String,
        slot: Option<String>,
    },
    #[serde(rename = "context")]
    Context {
        trigger: String,
        node: u64,
        id: String,
        slot: Option<String>,
        x: f64,
        y: f64,
    },
    #[serde(rename = "key")]
    Key {
        trigger: String,
        node: u64,
        id: String,
        slot: Option<String>,
        key: String,
        keycode: u32,
        modifiers: u32,
        pressed: bool,
    },
    #[serde(rename = "focus")]
    Focus {
        trigger: String,
        node: u64,
        id: String,
        slot: Option<String>,
        focused: bool,
    },
    #[serde(rename = "pointer")]
    Pointer {
        trigger: String,
        node: u64,
        id: String,
        slot: Option<String>,
        phase: UiPointerPhase,
        x: Option<f64>,
        y: Option<f64>,
        button: u32,
        modifiers: u32,
    },
    #[serde(rename = "drag")]
    Drag {
        trigger: String,
        node: u64,
        id: String,
        slot: Option<String>,
    },
    #[serde(rename = "drop")]
    Drop {
        trigger: String,
        node: u64,
        id: String,
        slot: Option<String>,
        source: u64,
        x: f64,
        y: f64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum UiPointerPhase {
    #[serde(rename = "enter")]
    Enter,
    #[serde(rename = "motion")]
    Motion,
    #[serde(rename = "leave")]
    Leave,
    #[serde(rename = "press")]
    Press,
    #[serde(rename = "release")]
    Release,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct UiCollectionSelection {
    pub source: u64,
    pub version: u64,
    pub rows: Vec<UiSelectedRow>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct UiSelectedRow {
    pub index: u64,
    pub id: String,
}

#[cfg(test)]
mod tests {
    use super::UiEvent;
    use serde_json::json;

    #[test]
    fn drop_has_the_exact_normalized_wire_shape_and_preserves_bound_trigger() {
        let event = UiEvent::Drop {
            trigger: "Activate".into(),
            node: 7,
            id: "producer-owned:event".into(),
            slot: Some("on_drop".into()),
            source: 4,
            x: 2.5,
            y: 8.0,
        };

        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({
                "interaction": "drop",
                "trigger": "Activate",
                "node": 7,
                "id": "producer-owned:event",
                "slot": "on_drop",
                "source": 4,
                "x": 2.5,
                "y": 8.0
            })
        );
    }

    #[test]
    fn variant_payload_is_required_and_unknown_fields_fail_closed() {
        assert!(
            serde_json::from_value::<UiEvent>(json!({
                "interaction": "drop",
                "trigger": "Drop",
                "node": 7,
                "id": "event",
                "slot": null,
                "x": 2.5,
                "y": 8.0
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<UiEvent>(json!({
                "interaction": "invoke",
                "trigger": "Activate",
                "node": 7,
                "id": "event",
                "slot": null,
                "invented": true
            }))
            .is_err()
        );
    }
}
