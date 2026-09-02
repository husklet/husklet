//! The one host service that lives on the window's thread rather than the host's.
//!
//! Every other port reaches a daemon and can be called from wherever the
//! extension is served. The terminal is widgets, and widgets may only be
//! touched from the main loop, so this is a relay instead of an
//! implementation: the port hands an [`Errand`] to whoever is drawing and waits
//! for that thread to answer.
//!
//! A relay rather than a channel of one-way requests, because the protocol's
//! terminal calls return values an extension acts on — the tab it just opened,
//! the pane a split produced — and a call that answered before the work
//! happened would hand back an identity nothing has yet.

use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::time::Duration;

use hl_extension::port::{
    Division, GridSize, HostError, PaneSemanticAction, PaneSemanticTree, PaneText, TabSummary, TerminalSurface,
    TerminalTopology,
};

/// How long a relayed call waits for the window to answer.
///
/// The window answers on its own tick, so this is many ticks of slack. It
/// exists so an extension is refused rather than held forever when the window
/// it was talking to has gone away mid-call.
pub const PATIENCE: Duration = Duration::from_secs(5);

/// How many errands may wait before the extension's thread blocks.
///
/// One in flight is the normal case, because each call blocks its own
/// conversation until it is answered; the slack is for several extensions
/// asking at once.
pub const CAPACITY: usize = 16;

/// What an extension asked the terminal for.
///
/// Not `Eq`: a split ratio is a measurement, and a measurement has no total
/// equality.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Every tab and the panes in it.
    Tabs,
    /// Nested tab and split topology.
    Topology,
    /// A new tab under this title.
    OpenTab(String),
    /// A pane split off the named slot.
    Split {
        /// The pane being divided.
        slot: String,
        /// Which way it is divided.
        division: Division,
    },
    /// A command run in the named slot.
    Spawn {
        /// The pane the command is typed into.
        slot: String,
        /// The command and its arguments.
        command: Vec<String>,
    },
    /// A bounded tail of what the named pane is showing.
    Read {
        /// The pane being read.
        slot: String,
        /// How many lines at most, already bounded by the protocol layer.
        lines: usize,
    },
    Semantics {
        slot: String,
    },
    SemanticAction {
        slot: String,
        action: PaneSemanticAction,
    },
    /// Raw bytes written to the named pane.
    Write {
        slot: String,
        contents: Vec<u8>,
    },
    /// Exact PTY grid requested for the named pane.
    ResizeGrid {
        slot: String,
        grid: GridSize,
    },
    /// The named pane, closed.
    Close {
        /// The pane being closed.
        slot: String,
    },
    /// Keyboard focus, moved to the named pane.
    Focus {
        /// The pane being focused.
        slot: String,
    },
    /// How much of its split the named pane takes.
    Ratio {
        /// The pane being resized.
        slot: String,
        /// The fraction of the split the pane takes.
        ratio: f64,
    },
    /// A pane split off the named slot, holding an extension's interface.
    Surface {
        /// Which extension will draw into it. `None` when the port was not
        /// attributed to one, which the window refuses rather than guesses at.
        origin: Option<String>,
        /// The pane being divided.
        slot: String,
        /// Which way it is divided.
        division: Division,
    },
}

/// What the window answered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Answer {
    /// The tabs, for [`Request::Tabs`].
    Tabs(Vec<TabSummary>),
    /// Nested layout, for [`Request::Topology`].
    Topology(TerminalTopology),
    /// The identity of what was opened or split.
    Slot(String),
    /// The text one pane is showing, for [`Request::Read`].
    Text(PaneText),
    Semantics(PaneSemanticTree),
    /// The work was done and names nothing.
    Done,
}

impl Answer {
    /// The failure an answer of the wrong shape is.
    ///
    /// The window's mistake rather than the extension's, so it is reported as a
    /// failure rather than as a refusal of what was asked for.
    fn mismatch(&self) -> HostError {
        HostError::Failed(format!("the terminal answered with {self:?}, which was not asked for"))
    }
}

/// One request and the line it is answered on.
pub struct Errand {
    request: Request,
    reply: SyncSender<Result<Answer, HostError>>,
}

impl Errand {
    /// What was asked for.
    #[must_use]
    pub const fn request(&self) -> &Request {
        &self.request
    }

    /// Answers it. An errand nobody is waiting for any more is not a failure:
    /// the caller timed out or its conversation ended.
    pub fn answer(self, answer: Result<Answer, HostError>) {
        let _ = self.reply.try_send(answer);
    }
}

impl std::fmt::Debug for Errand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Errand")
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

/// The end the window drains, on its own tick.
pub type Errands = Receiver<Errand>;

/// The terminal port as an extension holds it: a sender and a wait.
pub struct Relay {
    errands: SyncSender<Errand>,
    /// Which extension holds this port, when it was attributed to one. A pane
    /// that draws an interface has to name whose interface it draws, and the
    /// window cannot infer that from a request alone.
    origin: Option<String>,
}

impl Relay {
    /// Creates the port and the end the window drains.
    #[must_use]
    pub fn open() -> (Self, Errands) {
        let (errands, drained) = std::sync::mpsc::sync_channel(CAPACITY);
        (Self { errands, origin: None }, drained)
    }

    /// The same port, held by one named extension.
    #[must_use]
    pub fn of(&self, extension: &str) -> Self {
        Self {
            errands: self.errands.clone(),
            origin: Some(extension.to_owned()),
        }
    }

    /// Sends one request and waits out [`PATIENCE`] for the answer.
    fn ask(&self, request: Request) -> Result<Answer, HostError> {
        let (reply, answered) = std::sync::mpsc::sync_channel(1);
        let errand = Errand { request, reply };
        self.errands.try_send(errand).map_err(|_| unreachable())?;
        match answered.recv_timeout(PATIENCE) {
            Ok(answer) => answer,
            Err(RecvTimeoutError::Timeout) => Err(HostError::Failed(
                "the terminal did not answer within the time allowed".to_owned(),
            )),
            Err(RecvTimeoutError::Disconnected) => Err(unreachable()),
        }
    }

    /// The identity an opening or a split produced.
    fn slot(&self, request: Request) -> Result<String, HostError> {
        match self.ask(request)? {
            Answer::Slot(slot) => Ok(slot),
            other => Err(other.mismatch()),
        }
    }

    /// A request that changes a pane and names nothing back.
    fn done(&self, request: Request) -> Result<(), HostError> {
        match self.ask(request)? {
            Answer::Done => Ok(()),
            other => Err(other.mismatch()),
        }
    }
}

impl TerminalSurface for Relay {
    /// # Errors
    /// Returns a host failure when no window is drawing this workspace.
    fn tabs(&self) -> Result<Vec<TabSummary>, HostError> {
        match self.ask(Request::Tabs)? {
            Answer::Tabs(tabs) => Ok(tabs),
            other => Err(other.mismatch()),
        }
    }

    fn topology(&self) -> Result<TerminalTopology, HostError> {
        match self.ask(Request::Topology)? {
            Answer::Topology(topology) => Ok(topology),
            other => Err(other.mismatch()),
        }
    }

    /// # Errors
    /// Returns a host failure when no window is drawing this workspace.
    fn open_tab(&self, title: &str) -> Result<String, HostError> {
        self.slot(Request::OpenTab(title.to_owned()))
    }

    /// # Errors
    /// Returns a host failure when no window is drawing this workspace.
    fn split(&self, slot: &str, division: Division) -> Result<String, HostError> {
        self.slot(Request::Split {
            slot: slot.to_owned(),
            division,
        })
    }

    /// # Errors
    /// Returns a host failure when no window is drawing this workspace.
    fn spawn(&self, slot: &str, command: &[String]) -> Result<(), HostError> {
        self.done(Request::Spawn {
            slot: slot.to_owned(),
            command: command.to_vec(),
        })
    }

    /// # Errors
    /// Returns a host failure when no window is drawing this workspace.
    fn read(&self, slot: &str, lines: usize) -> Result<PaneText, HostError> {
        match self.ask(Request::Read {
            slot: slot.to_owned(),
            lines,
        })? {
            Answer::Text(text) => Ok(text),
            other => Err(other.mismatch()),
        }
    }

    fn semantics(&self, slot: &str) -> Result<PaneSemanticTree, HostError> {
        match self.ask(Request::Semantics { slot: slot.to_owned() })? {
            Answer::Semantics(tree) => Ok(tree),
            other => Err(other.mismatch()),
        }
    }

    fn semantic_action(&self, slot: &str, action: &PaneSemanticAction) -> Result<(), HostError> {
        self.done(Request::SemanticAction {
            slot: slot.to_owned(),
            action: action.clone(),
        })
    }

    fn write(&self, slot: &str, contents: &[u8]) -> Result<(), HostError> {
        self.done(Request::Write {
            slot: slot.to_owned(),
            contents: contents.to_vec(),
        })
    }

    fn resize_grid(&self, slot: &str, grid: GridSize) -> Result<(), HostError> {
        self.done(Request::ResizeGrid {
            slot: slot.to_owned(),
            grid,
        })
    }

    /// # Errors
    /// Returns a host failure when no window is drawing this workspace.
    fn close(&self, slot: &str) -> Result<(), HostError> {
        self.done(Request::Close { slot: slot.to_owned() })
    }

    /// # Errors
    /// Returns a host failure when no window is drawing this workspace.
    fn focus(&self, slot: &str) -> Result<(), HostError> {
        self.done(Request::Focus { slot: slot.to_owned() })
    }

    /// # Errors
    /// Returns a host failure when no window is drawing this workspace.
    fn ratio(&self, slot: &str, ratio: f64) -> Result<(), HostError> {
        self.done(Request::Ratio {
            slot: slot.to_owned(),
            ratio,
        })
    }

    /// # Errors
    /// Returns a host failure when no window is drawing this workspace, and a
    /// conflict when this port was not attributed to an extension.
    fn surface(&self, slot: &str, division: Division) -> Result<String, HostError> {
        self.slot(Request::Surface {
            origin: self.origin.clone(),
            slot: slot.to_owned(),
            division,
        })
    }
}

/// Said the same way whenever there is no window left to ask, so an extension
/// can recognize it rather than reading a different sentence per call.
fn unreachable() -> HostError {
    HostError::Failed("no window is drawing this workspace, so the terminal cannot be reached".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{Answer, Relay, Request};
    use hl_extension::port::{Division, GridSize, TerminalSurface as _};

    #[test]
    fn a_call_carries_its_request_and_takes_back_what_the_window_answered() {
        let (relay, errands) = Relay::open();
        let window = std::thread::spawn(move || {
            let errand = errands.recv().expect("an errand");
            assert_eq!(
                errand.request(),
                &Request::Split {
                    slot: "shell-1".to_owned(),
                    division: Division::Below,
                }
            );
            errand.answer(Ok(Answer::Slot("shell-2".to_owned())));
        });

        let produced = relay.split("shell-1", Division::Below).expect("a pane");

        assert_eq!(produced, "shell-2");
        window.join().expect("the window thread");
    }

    #[test]
    fn a_pane_that_draws_an_interface_names_whose_interface_it_draws() {
        let (relay, errands) = Relay::open();
        let window = std::thread::spawn(move || {
            let anonymous = errands.recv().expect("an errand");
            assert_eq!(
                anonymous.request(),
                &Request::Surface {
                    origin: None,
                    slot: "shell-1".to_owned(),
                    division: Division::Beside,
                }
            );
            anonymous.answer(Ok(Answer::Slot("pane-2".to_owned())));
            let attributed = errands.recv().expect("an errand");
            assert_eq!(
                attributed.request(),
                &Request::Surface {
                    origin: Some("sample".to_owned()),
                    slot: "shell-1".to_owned(),
                    division: Division::Beside,
                }
            );
            attributed.answer(Ok(Answer::Slot("pane-3".to_owned())));
        });

        drop(relay.surface("shell-1", Division::Beside).expect("a pane"));
        let named = relay.of("sample").surface("shell-1", Division::Beside).expect("a pane");

        assert_eq!(named, "pane-3");
        window.join().expect("the window thread");
    }

    #[test]
    fn raw_input_and_grid_cross_the_thread_boundary_unchanged() {
        let (relay, errands) = Relay::open();
        let window = std::thread::spawn(move || {
            let input = errands.recv().expect("input errand");
            assert_eq!(
                input.request(),
                &Request::Write {
                    slot: "shell-1".into(),
                    contents: b"printf x".to_vec()
                }
            );
            input.answer(Ok(Answer::Done));
            let grid = errands.recv().expect("grid errand");
            assert_eq!(
                grid.request(),
                &Request::ResizeGrid {
                    slot: "shell-1".into(),
                    grid: GridSize { columns: 120, rows: 40 }
                }
            );
            grid.answer(Ok(Answer::Done));
        });

        relay.write("shell-1", b"printf x").expect("input");
        relay
            .resize_grid("shell-1", GridSize { columns: 120, rows: 40 })
            .expect("grid");
        window.join().expect("window thread");
    }

    #[test]
    fn a_closed_window_refuses_rather_than_holding_the_extension() {
        let (relay, errands) = Relay::open();
        drop(errands);

        let refused = relay.open_tab("Logs").expect_err("no window");

        assert!(refused.to_string().contains("no window"), "got {refused}");
    }

    #[test]
    fn an_answer_of_the_wrong_shape_is_the_windows_mistake() {
        let (relay, errands) = Relay::open();
        let window = std::thread::spawn(move || {
            let errand = errands.recv().expect("an errand");
            errand.answer(Ok(Answer::Done));
        });

        let refused = relay.open_tab("Logs").expect_err("the wrong answer");

        assert!(refused.to_string().contains("not asked for"), "got {refused}");
        window.join().expect("the window thread");
    }
}
