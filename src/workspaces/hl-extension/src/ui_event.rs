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
    #[serde(rename = "edit")]
    Edit {
        trigger: String,
        node: u64,
        id: String,
        slot: Option<String>,
        source: u64,
        version: u64,
        row: UiSelectedRow,
        column: String,
        value: String,
    },
    #[serde(rename = "sort")]
    Sort {
        trigger: String,
        node: u64,
        id: String,
        slot: Option<String>,
        source: u64,
        version: u64,
        column: String,
        descending: bool,
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

impl UiEvent {
    /// Converts one interactive toolkit report into its authoritative wire DTO.
    /// Row-window requests are host-internal and therefore have no UI event.
    #[must_use]
    pub fn of(event: &hl_gui::Event, slot: Option<&str>) -> Option<Self> {
        use hl_gui::Event;
        let slot = slot.map(str::to_owned);
        Some(match event {
            Event::Invoke { node, id } => Self::Invoke {
                trigger: "Invoke".into(),
                node: node.raw(),
                id: id.as_str().into(),
                slot,
            },
            Event::Activate { node, id } => Self::Invoke {
                trigger: "Activate".into(),
                node: node.raw(),
                id: id.as_str().into(),
                slot,
            },
            Event::Submit { node, id } => Self::Submit {
                trigger: "Submit".into(),
                node: node.raw(),
                id: id.as_str().into(),
                slot,
            },
            Event::Change { node, id, value } => Self::Change {
                trigger: "Change".into(),
                node: node.raw(),
                id: id.as_str().into(),
                slot,
                value: value.clone(),
            },
            Event::Toggle { node, id, value } => Self::Change {
                trigger: "Toggle".into(),
                node: node.raw(),
                id: id.as_str().into(),
                slot,
                value: value.clone(),
            },
            Event::Expand { node, id, value } => Self::Change {
                trigger: "Expand".into(),
                node: node.raw(),
                id: id.as_str().into(),
                slot,
                value: value.clone(),
            },
            Event::Select {
                node,
                id,
                rows,
                collection,
            } => Self::Select {
                trigger: "Select".into(),
                node: node.raw(),
                id: id.as_str().into(),
                slot,
                rows: rows.clone(),
                collection: collection.as_ref().map(|selected| UiCollectionSelection {
                    source: selected.source.raw(),
                    version: selected.version.raw(),
                    rows: selected
                        .rows
                        .iter()
                        .map(|row| UiSelectedRow {
                            index: row.index,
                            id: row.id.to_string(),
                        })
                        .collect(),
                }),
            },
            Event::Edit { node, id, edit } => Self::Edit {
                trigger: "Edit".into(),
                node: node.raw(),
                id: id.as_str().into(),
                slot,
                source: edit.source.raw(),
                version: edit.version.raw(),
                row: UiSelectedRow {
                    index: edit.row.index,
                    id: edit.row.id.to_string(),
                },
                column: edit.column.clone(),
                value: edit.value.clone(),
            },
            Event::Sort { node, id, sort } => Self::Sort {
                trigger: "Sort".into(),
                node: node.raw(),
                id: id.as_str().into(),
                slot,
                source: sort.source.raw(),
                version: sort.version.raw(),
                column: sort.column.clone(),
                descending: sort.descending,
            },
            Event::Scroll { node, id, dx, dy } => Self::Scroll {
                trigger: "Scroll".into(),
                node: node.raw(),
                id: id.as_str().into(),
                slot,
                dx: *dx,
                dy: *dy,
            },
            Event::Close { node, id } => Self::Close {
                trigger: "Close".into(),
                node: node.raw(),
                id: id.as_str().into(),
                slot,
            },
            Event::Context { node, id, x, y } => Self::Context {
                trigger: "Context".into(),
                node: node.raw(),
                id: id.as_str().into(),
                slot,
                x: *x,
                y: *y,
            },
            Event::Key {
                node,
                id,
                key,
                keycode,
                modifiers,
                pressed,
            } => Self::Key {
                trigger: "Key".into(),
                node: node.raw(),
                id: id.as_str().into(),
                slot,
                key: key.clone(),
                keycode: *keycode,
                modifiers: *modifiers,
                pressed: *pressed,
            },
            Event::Focus { node, id, focused } => Self::Focus {
                trigger: "Focus".into(),
                node: node.raw(),
                id: id.as_str().into(),
                slot,
                focused: *focused,
            },
            Event::Pointer {
                node,
                id,
                phase,
                x,
                y,
                button,
                modifiers,
            } => Self::Pointer {
                trigger: "Pointer".into(),
                node: node.raw(),
                id: id.as_str().into(),
                slot,
                phase: match phase {
                    hl_gui::PointerPhase::Enter => UiPointerPhase::Enter,
                    hl_gui::PointerPhase::Motion => UiPointerPhase::Motion,
                    hl_gui::PointerPhase::Leave => UiPointerPhase::Leave,
                    hl_gui::PointerPhase::Press => UiPointerPhase::Press,
                    hl_gui::PointerPhase::Release => UiPointerPhase::Release,
                },
                x: *x,
                y: *y,
                button: *button,
                modifiers: *modifiers,
            },
            Event::Drag { node, id } => Self::Drag {
                trigger: "Drag".into(),
                node: node.raw(),
                id: id.as_str().into(),
                slot,
            },
            Event::Drop { node, id, source, x, y } => Self::Drop {
                trigger: "Drop".into(),
                node: node.raw(),
                id: id.as_str().into(),
                slot,
                source: source.raw(),
                x: *x,
                y: *y,
            },
            Event::Rows(_) => return None,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{UiEvent, UiSelectedRow};
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
        assert!(serde_json::from_value::<UiEvent>(json!({
            "interaction": "drop",
            "trigger": "Drop",
            "node": 7,
            "id": "event",
            "slot": null,
            "x": 2.5,
            "y": 8.0
        }))
        .is_err());
        assert!(serde_json::from_value::<UiEvent>(json!({
            "interaction": "invoke",
            "trigger": "Activate",
            "node": 7,
            "id": "event",
            "slot": null,
            "invented": true
        }))
        .is_err());
    }

    #[test]
    fn edit_wire_shape_carries_versioned_row_authority() {
        let event = UiEvent::Edit {
            trigger: "Edit".into(),
            node: 3,
            id: "3:Edit".into(),
            slot: Some("pane".into()),
            source: 7,
            version: 11,
            row: UiSelectedRow {
                index: 9,
                id: "immutable-9".into(),
            },
            column: "name".into(),
            value: "renamed".into(),
        };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({
                "interaction":"edit", "trigger":"Edit", "node":3, "id":"3:Edit", "slot":"pane",
                "source":7, "version":11, "row":{"index":9,"id":"immutable-9"}, "column":"name", "value":"renamed"
            })
        );
    }

    #[test]
    fn sort_wire_shape_carries_versioned_source_authority() {
        let event = UiEvent::Sort {
            trigger: "Sort".into(),
            node: 3,
            id: "3:Sort".into(),
            slot: Some("pane".into()),
            source: 7,
            version: 11,
            column: "name".into(),
            descending: true,
        };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({
                "interaction":"sort", "trigger":"Sort", "node":3, "id":"3:Sort", "slot":"pane",
                "source":7, "version":11, "column":"name", "descending":true
            })
        );
    }
}
