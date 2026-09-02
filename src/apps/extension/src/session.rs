//! The extension's side of the conversation.

use std::io::{Read, Write};

use hl_extension::{codec, Frame, Hello, Kind, Reply, Request, Transit, Welcome, Wire, PROTOCOL};

use crate::Extension;

/// Why the conversation ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// The host closed the socket, which is how a session normally ends.
    Closed,
    /// The host speaks a different protocol.
    Mismatch { host: u32, extension: u32 },
    /// A message could not be encoded or decoded.
    Malformed(String),
    /// The transport failed.
    Transit(String),
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(formatter, "the host closed the connection"),
            Self::Mismatch { host, extension } => {
                write!(
                    formatter,
                    "the host speaks protocol {host}, this extension speaks {extension}"
                )
            }
            Self::Malformed(detail) => write!(formatter, "malformed message: {detail}"),
            Self::Transit(detail) => write!(formatter, "transport failed: {detail}"),
        }
    }
}

impl std::error::Error for Outcome {}

impl From<Transit> for Outcome {
    fn from(transit: Transit) -> Self {
        match transit {
            Transit::Closed => Self::Closed,
            Transit::Pending => Self::Transit("the host did not finish a frame".into()),
            Transit::Malformed(malformed) => Self::Malformed(malformed.to_string()),
            Transit::Io(detail) => Self::Transit(detail),
        }
    }
}

/// Serves one connection until the host closes it.
///
/// # Errors
/// Returns why the conversation ended.
pub fn serve<S: Read + Write>(stream: S, extension: Extension) -> Result<(), Outcome> {
    let mut wire = Wire::new(stream);
    let mut extension = extension;
    greet(&mut wire)?;
    for request in crate::opening() {
        send(&mut wire, &request)?;
    }
    loop {
        let frame = match wire.receive() {
            Ok(frame) => frame,
            Err(Transit::Closed) => return Ok(()),
            Err(other) => return Err(other.into()),
        };
        answer(&mut wire, &mut extension, &frame)?;
    }
}

/// Reads the host's opening frame and replies, so the host knows which
/// protocol this extension speaks before it is asked for anything.
fn greet<S: Read + Write>(wire: &mut Wire<S>) -> Result<Welcome, Outcome> {
    let frame = wire.receive()?;
    let welcome: Welcome = decode(&frame)?;
    if welcome.protocol != PROTOCOL {
        return Err(Outcome::Mismatch {
            host: welcome.protocol,
            extension: PROTOCOL,
        });
    }
    let hello = Hello {
        protocol: PROTOCOL,
        name: welcome.peer.clone(),
        features: Vec::new(),
    };
    let payload = encode(&hello)?;
    wire.send(&Frame::control(Kind::Response, payload))?;
    Ok(welcome)
}

/// Handles one frame from the host.
fn answer<S: Read + Write>(wire: &mut Wire<S>, extension: &mut Extension, frame: &Frame) -> Result<(), Outcome> {
    if frame.kind == Kind::Event {
        return observe(wire, extension, frame);
    }
    let Ok(reply) = serde_json::from_slice::<Reply>(&frame.payload) else {
        // A failure or an unmodelled reply is not fatal: the host has already
        // said why, and the extension keeps serving what it still may.
        return Ok(());
    };
    let Reply::Containers(containers) = reply else {
        return Ok(());
    };
    for request in extension.observe(containers) {
        send(wire, &request)?;
    }
    Ok(())
}

/// Handles a pushed event: either fresh containers or a window to answer.
fn observe<S: Read + Write>(wire: &mut Wire<S>, extension: &mut Extension, frame: &Frame) -> Result<(), Outcome> {
    if let Ok(request) = serde_json::from_slice::<hl_gui::RowRequest>(&frame.payload) {
        let window = extension.answer(&request);
        let payload = encode(&window)?;
        wire.send(&Frame::new(frame.channel, Kind::Response, payload))?;
        return Ok(());
    }
    let Ok(containers) = serde_json::from_slice(&frame.payload) else {
        return Ok(());
    };
    for request in extension.observe(containers) {
        send(wire, &request)?;
    }
    Ok(())
}

fn send<S: Write>(wire: &mut Wire<S>, request: &Request) -> Result<(), Outcome> {
    let frame = codec::request(request).map_err(|error| Outcome::Malformed(error.to_string()))?;
    wire.send(&frame)?;
    Ok(())
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, Outcome> {
    serde_json::to_vec(value).map_err(|error| Outcome::Malformed(error.to_string()))
}

fn decode<T: serde::de::DeserializeOwned>(frame: &Frame) -> Result<T, Outcome> {
    serde_json::from_slice(&frame.payload).map_err(|error| Outcome::Malformed(error.to_string()))
}
