//! The host's half of the conversation: what it says, and what it says it on.
//!
//! An extension is read on the thread serving its socket and spoken to on the
//! driver's thread, so the writing end lives here as a second descriptor rather
//! than inside the conversation that owns the reading end.
//!
//! The protocol models no interaction message of its own yet, so what a person
//! did is encoded here: a row request as itself, because that is the shape both
//! ends already agree on, and everything else in an envelope naming it.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, PoisonError};

use hl_ws_extension::{Frame, Kind, Wire};

use super::EVENTS;

/// Tells the extension what a person did.
///
/// A row request travels as itself, because that is the shape both ends of the
/// protocol already agree on; every other interaction travels in an envelope
/// naming it, since the protocol models no interaction message of its own yet.
pub(super) fn speak(voice: &Voice, event: &hl_gui::Event) {
    let Some(payload) = carriage(event) else {
        return;
    };
    voice.say(&Frame::new(EVENTS, Kind::Event, payload));
}

/// Encodes one interaction. `None` when it cannot be encoded, which is not
/// worth ending a conversation over.
fn carriage(event: &hl_gui::Event) -> Option<Vec<u8>> {
    let value = match event {
        hl_gui::Event::Rows(request) => serde_json::to_value(request).ok()?,
        hl_gui::Event::Invoke { node, id } => envelope("invoke", *node, id),
        hl_gui::Event::Submit { node, id } => envelope("submit", *node, id),
        hl_gui::Event::Change { node, id, value } => change(*node, id, value),
        hl_gui::Event::Select { node, id, rows } => selection(*node, id, rows),
        _ => return None,
    };
    serde_json::to_vec(&value).ok()
}

/// The shape every interaction is sent in.
fn envelope(interaction: &str, node: hl_gui::NodeId, id: &hl_gui::EventId) -> serde_json::Value {
    serde_json::json!({ "interaction": interaction, "node": node, "id": id })
}

/// A changed value, added to the envelope it belongs in.
fn change(node: hl_gui::NodeId, id: &hl_gui::EventId, value: &hl_gui::PropValue) -> serde_json::Value {
    let mut carried = envelope("change", node, id);
    insert(&mut carried, "value", serde_json::to_value(value).ok());
    carried
}

/// A row selection, added to the envelope it belongs in.
fn selection(node: hl_gui::NodeId, id: &hl_gui::EventId, rows: &[u64]) -> serde_json::Value {
    let mut carried = envelope("select", node, id);
    insert(&mut carried, "rows", serde_json::to_value(rows).ok());
    carried
}

/// Adds one field to an envelope, leaving it alone when there is nothing to add.
fn insert(carried: &mut serde_json::Value, field: &str, value: Option<serde_json::Value>) {
    let (Some(object), Some(value)) = (carried.as_object_mut(), value) else {
        return;
    };
    object.insert(field.to_owned(), value);
}

/// The host's writing end of the live conversation.
///
/// A second descriptor for the same socket, because [`Conversation`] owns the
/// stream and answers calls on the thread serving it, while interaction is
/// handed over on the driver's thread.
#[derive(Clone, Default)]
pub(super) struct Voice {
    held: Arc<Mutex<Option<Wire<UnixStream>>>>,
}

impl Voice {
    /// Takes the writing end of a connection that has just been accepted.
    pub(super) fn hold(&self, stream: UnixStream) {
        self.wire().replace(Wire::new(stream));
    }

    /// Gives it up when the conversation ends.
    pub(super) fn release(&self) {
        self.wire().take();
    }

    /// Writes one frame, if there is anyone to write to.
    ///
    /// A failed write is dropped rather than reported: the conversation on the
    /// other thread is reading the same socket and will report the ending, and
    /// two reports of one hangup would show a person the second one.
    pub(super) fn say(&self, frame: &Frame) {
        let mut held = self.wire();
        let Some(wire) = held.as_mut() else {
            return;
        };
        let _ = wire.send(frame);
    }

    fn wire(&self) -> std::sync::MutexGuard<'_, Option<Wire<UnixStream>>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
