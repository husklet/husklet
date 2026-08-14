//! A producer that serves the whole component catalogue over the protocol.
//!
//! The reference extension proves one real interface crosses a socket. This
//! proves every component in the library does, by sending the same catalogue
//! the in-process mode renders and letting the host rebuild it from mutations
//! alone.

use std::io::{Read, Write};

use hl_ws_extension::{ChannelId, Frame, Hello, Kind, Request, Transit, Welcome, Wire, PROTOCOL};

use crate::Catalogue;

/// Serves the catalogue until the host stops asking.
///
/// # Errors
/// Returns why the conversation ended, except a clean close, which is how a
/// finished conversation normally ends.
pub fn serve<S: Read + Write>(stream: S, filter: Option<&str>) -> Result<(), Transit> {
    let mut wire = Wire::new(stream);
    greet(&mut wire)?;
    open(&mut wire)?;
    describe(&mut wire, filter)?;
    answer(&mut wire)
}

/// Reads the host's opening frame and replies with this producer's protocol.
fn greet<S: Read + Write>(wire: &mut Wire<S>) -> Result<(), Transit> {
    let frame = wire.receive()?;
    let Ok(welcome) = serde_json::from_slice::<Welcome>(&frame.payload) else {
        return Ok(());
    };
    let hello = Hello {
        protocol: PROTOCOL,
        name: welcome.extension,
        features: Vec::new(),
    };
    send(wire, Kind::Response, &hello)
}

fn open<S: Read + Write>(wire: &mut Wire<S>) -> Result<(), Transit> {
    let request = Request::InterfaceOpenTab {
        title: "Catalogue".into(),
    };
    call(wire, &request)
}

/// Sends the catalogue as one interface, then says how long its table is.
fn describe<S: Read + Write>(wire: &mut Wire<S>, filter: Option<&str>) -> Result<(), Transit> {
    let (_, frame) = Catalogue::selected(filter);
    call(wire, &Request::InterfaceRender { frame })?;
    // Every source the interface names needs a length; a table whose source is
    // never described has nothing to ask for and stays empty.
    for source in crate::sources() {
        call(
            wire,
            &Request::SourceResize {
                mutation: hl_gui::SourceMutation::Length {
                    source,
                    version: hl_gui::Version::new(1),
                    rows: crate::ROWS,
                },
            },
        )?;
    }
    Ok(())
}

/// Answers row windows until the host closes the socket.
fn answer<S: Read + Write>(wire: &mut Wire<S>) -> Result<(), Transit> {
    loop {
        let frame = match wire.receive() {
            Ok(frame) => frame,
            Err(Transit::Closed) => return Ok(()),
            Err(other) => return Err(other),
        };
        let Ok(request) = serde_json::from_slice::<hl_gui::RowRequest>(&frame.payload) else {
            continue;
        };
        let window = crate::answer(&request);
        send(wire, Kind::Response, &window)?;
    }
}

/// Sends a call and waits for its answer, so the host and producer stay in
/// step rather than both writing into a socket neither is reading.
fn call<S: Read + Write>(wire: &mut Wire<S>, request: &Request) -> Result<(), Transit> {
    send(wire, Kind::Request, request)?;
    wire.receive().map(|_| ())
}

fn send<S: Write, T: serde::Serialize>(wire: &mut Wire<S>, kind: Kind, value: &T) -> Result<(), Transit> {
    let payload = serde_json::to_vec(value).unwrap_or_default();
    wire.send(&Frame::new(ChannelId::new(1), kind, payload))
}
