//! Where the page hands back everything the user and the tables ask for.

use hl_gui::Event;

/// Something the page has for whoever is hosting the extension.
#[derive(Clone, Debug, PartialEq)]
pub enum Signal {
    /// Interaction reported by the rendered surface, or a table asking for a
    /// window of rows — both arrive as [`Event`], so both travel one path.
    Interaction(Event),
    /// Interaction from one independently retained pane surface.
    InteractionAt { slot: String, event: Event },
    /// The user asked for the stopped extension to be started again.
    Retry,
}

/// The page's only outbound edge.
///
/// Narrow on purpose: the page never names the extension host, so the host can
/// change shape without touching this screen and the screen stays testable
/// against a plain closure.
pub trait Sink {
    /// Accepts one signal. Called on the main loop, so it must not block.
    fn accept(&self, signal: Signal);
}

impl<F: Fn(Signal)> Sink for F {
    fn accept(&self, signal: Signal) {
        self(signal);
    }
}
