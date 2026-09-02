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

use crate::request::{Failure, Reply, Request};
use crate::Welcome;

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
    Ok(Frame::new(ChannelId::CONTROL, Kind::Request, payload(welcome)?))
}

/// Decodes the host's opening frame.
///
/// # Errors
/// Returns `Coding::Malformed` when the frame is not a control request or its
/// payload is not a `Welcome`.
pub fn read_welcome(frame: &Frame) -> Result<Welcome, Coding> {
    expect(frame, Kind::Request, ChannelId::CONTROL)?;
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
    let (name, node, id, detail) = match event {
        Event::Invoke { node, id } => ("invoke", node, id, serde_json::Value::Null),
        Event::Submit { node, id } => ("submit", node, id, serde_json::Value::Null),
        Event::Change { node, id, value } => ("change", node, id, serde_json::json!({ "value": value })),
        Event::Select { node, id, rows } => ("select", node, id, serde_json::json!({ "rows": rows })),
        Event::Focus { node, id, focused } => ("focus", node, id, serde_json::json!({ "focused": focused })),
        _ => return None,
    };
    let mut trigger = name.to_owned();
    trigger.get_mut(0..1)?.make_ascii_uppercase();
    let mut value = serde_json::json!({
        "interaction": name, "trigger": trigger, "node": node, "id": id,
    });
    if let (Some(target), Some(fields)) = (value.as_object_mut(), detail.as_object()) {
        target.extend(fields.clone());
    }
    if let (Some(slot), Some(target)) = (slot, value.as_object_mut()) {
        target.insert("slot".into(), serde_json::Value::String(slot.to_owned()));
    }
    serde_json::to_vec(&value).ok()
}

fn payload<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, Coding> {
    hl_rpc::payload(value)
}

fn parse<T: serde::de::DeserializeOwned>(frame: &Frame) -> Result<T, Coding> {
    serde_json::from_slice(&frame.payload).map_err(|error| Coding::Malformed(error.to_string()))
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
