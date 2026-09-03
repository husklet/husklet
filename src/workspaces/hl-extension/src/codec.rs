//! Protocol messages as frame payloads.
//!
//! Framing says how bytes are delimited and [`Request`] says what a call means;
//! this is the one place that joins them. Keeping the join here means the frame
//! kind, the channel, and the error flag are chosen once, from the message
//! itself, rather than at each call site where they could disagree.
//!
//! JSON is the payload encoding because an extension may be written in any
//! language, and the schema is already declared by the serde attributes on the
//! message types rather than restated here.

use hl_rpc::{ChannelId, Flags, Frame, Hello, Kind};

pub use hl_rpc::Coding;

use crate::Welcome;
use crate::request::{Failure, Reply, Request};

/// The channel calls and their answers ride on.
///
/// Host-opened and therefore even. The handshake stays on
/// [`ChannelId::CONTROL`] so a peer that has not agreed a version yet cannot
/// have opened anything else.
pub const CALLS: ChannelId = ChannelId::new(2);

/// Encodes the host's opening frame.
///
/// # Errors
/// Returns `Coding::Oversize` when the encoded message exceeds the payload
/// limit, and `Coding::Malformed` when it cannot be serialized.
pub fn welcome(welcome: &Welcome) -> Result<Frame, Coding> {
    Ok(Frame::new(ChannelId::CONTROL, Kind::Open, payload(welcome)?))
}

/// Decodes the host's opening frame.
///
/// # Errors
/// Returns `Coding::Malformed` when the frame is not a control request or its
/// payload is not a `Welcome`.
pub fn read_welcome(frame: &Frame) -> Result<Welcome, Coding> {
    expect(frame, Kind::Open, ChannelId::CONTROL)?;
    parse(frame)
}

/// Encodes the extension's reply to the opening frame.
///
/// # Errors
/// Returns `Coding::Oversize` when the encoded message exceeds the payload
/// limit, and `Coding::Malformed` when it cannot be serialized.
pub fn hello(hello: &Hello) -> Result<Frame, Coding> {
    Ok(Frame::new(ChannelId::CONTROL, Kind::Response, payload(hello)?))
}

/// Decodes the extension's reply to the opening frame.
///
/// # Errors
/// Returns `Coding::Malformed` when the frame is not a control response or its
/// payload is not a `Hello`.
pub fn read_hello(frame: &Frame) -> Result<Hello, Coding> {
    expect(frame, Kind::Response, ChannelId::CONTROL)?;
    parse(frame)
}

/// Encodes a call from an extension.
///
/// # Errors
/// Returns `Coding::Oversize` when the encoded call exceeds the payload limit,
/// which is how an interface too large to send is refused rather than
/// truncated, and `Coding::Malformed` when it cannot be serialized.
pub fn request(request: &Request) -> Result<Frame, Coding> {
    Ok(Frame::new(CALLS, Kind::Request, payload(request)?))
}

/// Decodes a call from an extension.
///
/// # Errors
/// Returns `Coding::Malformed` when the frame is not a request on the call
/// channel, or names a call this host does not implement.
pub fn read_request(frame: &Frame) -> Result<Request, Coding> {
    expect(frame, Kind::Request, CALLS)?;
    if !frame.flags.has(Flags::END) || frame.flags.has(Flags::ERROR) || frame.flags.has(Flags::COALESCED) {
        return Err(Coding::Malformed(
            "a call must be one complete, unflagged request".into(),
        ));
    }
    parse(frame)
}

/// Encodes a successful answer.
///
/// # Errors
/// Returns `Coding::Oversize` when the encoded answer exceeds the payload
/// limit, and `Coding::Malformed` when it cannot be serialized.
pub fn reply(reply: &Reply) -> Result<Frame, Coding> {
    Ok(Frame::new(CALLS, Kind::Response, payload(reply)?))
}

/// Decodes a successful answer.
///
/// # Errors
/// Returns `Coding::Malformed` when the frame is not a response on the call
/// channel, when it is flagged as a failure, or when its payload is not a
/// `Reply`.
pub fn read_reply(frame: &Frame) -> Result<Reply, Coding> {
    expect(frame, Kind::Response, CALLS)?;
    if frame.flags.has(Flags::ERROR) {
        return Err(Coding::Malformed("a failure was read as a reply".into()));
    }
    parse(frame)
}

/// Encodes a refusal or a host error.
///
/// The error flag rather than the payload shape is what marks a failure, so a
/// receiver knows which of the two to parse before parsing either.
///
/// # Errors
/// Returns `Coding::Oversize` when the encoded failure exceeds the payload
/// limit, and `Coding::Malformed` when it cannot be serialized.
pub fn failure(failure: &Failure) -> Result<Frame, Coding> {
    Ok(Frame::new(CALLS, Kind::Response, payload(failure)?).flagged(Flags::ERROR))
}

/// Decodes a refusal or a host error.
///
/// # Errors
/// Returns `Coding::Malformed` when the frame is not a response on the call
/// channel, when it is not flagged as a failure, or when its payload is not a
/// `Failure`.
pub fn read_failure(frame: &Frame) -> Result<Failure, Coding> {
    expect(frame, Kind::Response, CALLS)?;
    if !frame.flags.has(Flags::ERROR) {
        return Err(Coding::Malformed("a reply was read as a failure".into()));
    }
    parse(frame)
}

/// Whether a frame carries a failure rather than a result, so a receiver can
/// choose between [`read_reply`] and [`read_failure`] without guessing.
#[must_use]
pub fn is_failure(frame: &Frame) -> bool {
    frame.flags.has(Flags::ERROR)
}

/// Encodes a toolkit interaction in the event envelope consumed by extension
/// sessions. Kept beside the request codec so production and E2E hosts cannot
/// drift into subtly different node, handler, or addressed-slot spellings.
#[must_use]
pub fn interaction(event: &hl_gui::Event, slot: Option<&str>) -> Option<Vec<u8>> {
    use hl_gui::Event;
    let (name, trigger, node, id, detail) = match event {
        Event::Invoke { node, id } => ("invoke", "Invoke", node, id, serde_json::Value::Null),
        Event::Activate { node, id } => ("invoke", "Activate", node, id, serde_json::Value::Null),
        Event::Submit { node, id } => ("submit", "Submit", node, id, serde_json::Value::Null),
        Event::Change { node, id, value } => ("change", "Change", node, id, serde_json::json!({ "value": value })),
        Event::Toggle { node, id, value } => ("change", "Toggle", node, id, serde_json::json!({ "value": value })),
        Event::Expand { node, id, value } => ("change", "Expand", node, id, serde_json::json!({ "value": value })),
        Event::Select {
            node,
            id,
            rows,
            collection,
        } => (
            "select", "Select",
            node,
            id,
            serde_json::json!({ "rows": rows, "collection": collection.as_ref().map(|selected| serde_json::json!({
                "source": selected.source.raw(),
                "version": selected.version.raw(),
                "rows": selected.rows.iter().map(|row| serde_json::json!({ "index": row.index, "id": row.id.to_string() })).collect::<Vec<_>>(),
            })) }),
        ),
        Event::Scroll { node, id, dx, dy } => ("scroll", "Scroll", node, id, serde_json::json!({ "dx": dx, "dy": dy })),
        Event::Close { node, id } => ("close", "Close", node, id, serde_json::Value::Null),
        Event::Context { node, id, x, y } => ("context", "Context", node, id, serde_json::json!({ "x": x, "y": y })),
        Event::Key {
            node,
            id,
            key,
            keycode,
            modifiers,
            pressed,
        } => (
            "key", "Key",
            node,
            id,
            serde_json::json!({
                "key": key,
                "keycode": keycode,
                "modifiers": modifiers,
                "pressed": pressed,
            }),
        ),
        Event::Focus { node, id, focused } => ("focus", "Focus", node, id, serde_json::json!({ "focused": focused })),
        Event::Pointer {
            node,
            id,
            phase,
            x,
            y,
            button,
            modifiers,
        } => (
            "pointer", "Pointer",
            node,
            id,
            serde_json::json!({
                "phase": match phase {
                    hl_gui::PointerPhase::Enter => "enter",
                    hl_gui::PointerPhase::Motion => "motion",
                    hl_gui::PointerPhase::Leave => "leave",
                    hl_gui::PointerPhase::Press => "press",
                    hl_gui::PointerPhase::Release => "release",
                },
                "x": x,
                "y": y,
                "button": button,
                "modifiers": modifiers,
            }),
        ),
        Event::Drag { node, id } => ("drag", "Drag", node, id, serde_json::Value::Null),
        Event::Drop { node, id, source, x, y } => (
            "drop", "Drop",
            node,
            id,
            serde_json::json!({ "source": source, "x": x, "y": y }),
        ),
        _ => return None,
    };
    let mut value = serde_json::json!({
        "interaction": name, "trigger": trigger, "node": node, "id": id,
    });
    if let (Some(target), Some(fields)) = (value.as_object_mut(), detail.as_object()) {
        target.extend(fields.clone());
    }
    if let (Some(slot), Some(target)) = (slot, value.as_object_mut()) {
        target.insert("slot".into(), serde_json::Value::String(slot.to_owned()));
    }
    payload(&value).ok()
}

/// Encodes a cross-language JSON payload while refusing integers JavaScript
/// cannot distinguish exactly.
///
/// # Errors
/// Returns [`Coding::Malformed`] for an unsafe integer or serialization error,
/// and [`Coding::Oversize`] when the framed payload bound is exceeded.
pub fn payload<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, Coding> {
    let value = serde_json::to_value(value).map_err(|error| Coding::Malformed(error.to_string()))?;
    safe_numbers(&value)?;
    hl_rpc::payload(&value)
}

fn parse<T: serde::de::DeserializeOwned>(frame: &Frame) -> Result<T, Coding> {
    let value: serde_json::Value =
        serde_json::from_slice(&frame.payload).map_err(|error| Coding::Malformed(error.to_string()))?;
    safe_numbers(&value)?;
    serde_json::from_value(value).map_err(|error| Coding::Malformed(error.to_string()))
}

fn safe_numbers(value: &serde_json::Value) -> Result<(), Coding> {
    match value {
        serde_json::Value::Number(number) => {
            let unsafe_integer = number
                .as_u64()
                .is_some_and(|value| value > crate::JSON_SAFE_INTEGER_MAX)
                || number.as_i64().is_some_and(|value| {
                    value < -(crate::JSON_SAFE_INTEGER_MAX as i64) || value > crate::JSON_SAFE_INTEGER_MAX as i64
                });
            if unsafe_integer {
                return Err(Coding::Malformed(format!(
                    "integer {number} exceeds the lossless JSON boundary {}",
                    crate::JSON_SAFE_INTEGER_MAX
                )));
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                safe_numbers(value)?;
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values() {
                safe_numbers(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn expect(frame: &Frame, kind: Kind, channel: ChannelId) -> Result<(), Coding> {
    if frame.kind != kind {
        return Err(Coding::Malformed(format!(
            "expected a {kind:?} frame, received a {:?} frame",
            frame.kind
        )));
    }
    if frame.channel != channel {
        return Err(Coding::Malformed(format!(
            "expected channel {}, received channel {}",
            channel.raw(),
            frame.channel.raw()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{interaction, read_request, request, safe_numbers, welcome};
    use crate::{
        Capability, ChannelId, ExtensionName, Frame, Grant, JSON_SAFE_INTEGER_MAX, Kind, Limits, PaneChange,
        PaneChangeKind, Request, Snapshot, Welcome,
    };
    use hl_gui::{CollectionSelection, Event, EventId, NodeId, SelectedRow, SourceId, Version};

    #[test]
    fn host_greeting_uses_the_distinct_open_control_frame() {
        let frame = welcome(&Welcome {
            protocol: crate::PROTOCOL,
            host: "husklet".into(),
            workspace: "dev".into(),
            peer: ExtensionName::new("fixture").unwrap(),
            granted: Grant::new([Capability::WorkspaceRead]),
            limits: Limits::default(),
        })
        .expect("welcome encodes");
        assert_eq!(frame.channel, ChannelId::CONTROL);
        assert_eq!(frame.kind, Kind::Open);
    }

    #[test]
    fn collection_selection_encodes_generation_and_producer_identity() {
        let payload = interaction(
            &Event::Select {
                node: NodeId::new(9),
                id: EventId::new("9:Select"),
                rows: vec![42],
                collection: Some(CollectionSelection {
                    source: SourceId::new(7),
                    version: Version::new(3),
                    rows: vec![SelectedRow { index: 42, id: 90_042 }],
                }),
            },
            Some("surface-1"),
        )
        .expect("selection is encodable");
        let value: serde_json::Value = serde_json::from_slice(&payload).expect("JSON event");
        assert_eq!(value["rows"], serde_json::json!([42]));
        assert_eq!(
            value["collection"],
            serde_json::json!({
                "source": 7, "version": 3, "rows": [{ "index": 42, "id": "90042" }]
            })
        );
    }

    #[test]
    fn aliased_toolkit_signals_preserve_the_bound_trigger_and_opaque_event_id() {
        let node = NodeId::new(7);
        for (event, interaction_name, trigger) in [
            (
                Event::Activate { node, id: EventId::new("opaque/not-a-trigger") },
                "invoke",
                "Activate",
            ),
            (
                Event::Toggle { node, id: EventId::new("opaque/not-a-trigger"), value: hl_gui::PropValue::Flag(true) },
                "change",
                "Toggle",
            ),
            (
                Event::Expand { node, id: EventId::new("opaque/not-a-trigger"), value: hl_gui::PropValue::Flag(true) },
                "change",
                "Expand",
            ),
        ] {
            let payload = interaction(&event, Some("pane-7")).expect("interaction payload");
            let value: serde_json::Value = serde_json::from_slice(&payload).expect("JSON event");
            assert_eq!(value["interaction"], interaction_name);
            assert_eq!(value["trigger"], trigger);
            assert_eq!(value["id"], "opaque/not-a-trigger");
        }
    }

    #[test]
    fn every_interactive_toolkit_event_crosses_the_socket_with_its_detail() {
        let node = NodeId::new(9);
        let id = || EventId::new("9:event");
        let cases = [
            (
                Event::Scroll {
                    node,
                    id: id(),
                    dx: 1.25,
                    dy: -2.5,
                },
                "scroll",
                serde_json::json!({ "dx": 1.25, "dy": -2.5 }),
            ),
            (Event::Close { node, id: id() }, "close", serde_json::json!({})),
            (
                Event::Context {
                    node,
                    id: id(),
                    x: 12.0,
                    y: 34.0,
                },
                "context",
                serde_json::json!({ "x": 12.0, "y": 34.0 }),
            ),
            (
                Event::Key {
                    node,
                    id: id(),
                    key: "Return".into(),
                    keycode: 36,
                    modifiers: 5,
                    pressed: true,
                },
                "key",
                serde_json::json!({ "key": "Return", "keycode": 36, "modifiers": 5, "pressed": true }),
            ),
            (
                Event::Pointer {
                    node,
                    id: id(),
                    phase: hl_gui::PointerPhase::Release,
                    x: Some(4.0),
                    y: None,
                    button: 1,
                    modifiers: 2,
                },
                "pointer",
                serde_json::json!({ "phase": "release", "x": 4.0, "y": null, "button": 1, "modifiers": 2 }),
            ),
            (Event::Drag { node, id: id() }, "drag", serde_json::json!({})),
            (
                Event::Drop {
                    node,
                    id: id(),
                    source: NodeId::new(4),
                    x: 7.0,
                    y: 8.0,
                },
                "drop",
                serde_json::json!({ "source": 4, "x": 7.0, "y": 8.0 }),
            ),
        ];
        for (event, name, detail) in cases {
            let payload = interaction(&event, Some("surface-1")).expect("event is socket-visible");
            let value: serde_json::Value = serde_json::from_slice(&payload).expect("event JSON");
            assert_eq!(value["interaction"], name);
            assert_eq!(
                value["trigger"],
                format!("{}{}", name[..1].to_ascii_uppercase(), &name[1..])
            );
            assert_eq!(value["node"], 9);
            assert_eq!(value["id"], "9:event");
            assert_eq!(value["slot"], "surface-1");
            for (field, expected) in detail.as_object().unwrap() {
                assert_eq!(&value[field], expected, "{name}.{field}");
            }
        }
    }

    #[test]
    fn every_typed_payload_refuses_integers_javascript_cannot_preserve() {
        let boundary = Request::ExtensionAcquisitionCancel {
            job: "job-1".into(),
            revision: JSON_SAFE_INTEGER_MAX,
        };
        assert_eq!(
            read_request(&request(&boundary).expect("safe boundary")).unwrap(),
            boundary
        );

        let unsafe_outbound = Request::ExtensionAcquisitionCancel {
            job: "job-1".into(),
            revision: JSON_SAFE_INTEGER_MAX + 1,
        };
        assert!(
            request(&unsafe_outbound)
                .unwrap_err()
                .to_string()
                .contains("lossless JSON boundary")
        );

        let unsafe_inbound = Frame::new(
            ChannelId::new(2),
            Kind::Request,
            format!(
                r#"{{"call":"extension_acquisition_cancel","with":{{"job":"job-1","revision":{}}}}}"#,
                JSON_SAFE_INTEGER_MAX + 1
            )
            .into_bytes(),
        );
        assert!(
            read_request(&unsafe_inbound)
                .unwrap_err()
                .to_string()
                .contains("lossless JSON boundary")
        );
        assert!(safe_numbers(&serde_json::json!(-(JSON_SAFE_INTEGER_MAX as i64) - 1)).is_err());
        assert!(
            Snapshot::PaneChanges(PaneChange {
                slot: "pane-1".into(),
                kind: PaneChangeKind::Terminal,
                revision: JSON_SAFE_INTEGER_MAX + 1,
                generation: 1,
                coalesced: 0,
            })
            .payload()
            .unwrap_err()
            .to_string()
            .contains("lossless JSON boundary")
        );
    }
}
