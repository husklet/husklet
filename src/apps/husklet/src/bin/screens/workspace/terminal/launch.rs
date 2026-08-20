use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChildStatus {
    Exited(i32),
    Signaled(i32),
    Unknown(i32),
}

struct Launch<'a> {
    window: &'a Rc<TermWin>,
    terminal: &'a vte4::Terminal,
    pid: &'a Rc<Cell<i32>>,
}

impl Launch<'_> {
    fn attach(&self, child: i32, pty: vte4::Pty) {
        self.pid.set(child);
        // Keep the FOREIGN pty sized to the terminal grid — VTE doesn't resize a foreign pty itself,
        // so without this htop is malformed / half-height and doesn't reflow on window resize.
        let weak = self.terminal.downgrade();
        let pid = self.pid.clone();
        let mut last = (0, 0);
        glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
            if pid.get() == 0 {
                return glib::ControlFlow::Break;
            }
            let Some(terminal) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let dimensions = (terminal.column_count() as i32, terminal.row_count() as i32);
            if dimensions.0 > 0 && dimensions.1 > 0 && dimensions != last {
                let _ = pty.set_size(dimensions.1, dimensions.0);
                last = dimensions;
            }
            glib::ControlFlow::Continue
        });
        self.watch_child(child);
        self.schedule_typed_text();
    }

    fn watch_child(&self, child: i32) {
        let window = self.window.clone();
        let terminal = self.terminal.clone();
        let pid = self.pid.clone();
        glib::child_watch_add_local(glib::Pid(child), move |_pid, status| {
            let status = ChildStatus::finish(&pid, status);
            if status.should_report(window.closing.get()) {
                terminal.feed(format!("\r\n\x1b[31mworkspace session ended ({status})\x1b[0m\r\n").as_bytes());
                return;
            }
            if !status.succeeded() {
                return;
            }
            PaneView::new(&window, &terminal).close();
        });
    }

    fn schedule_typed_text(&self) {
        if let Some(text) = AppConfig::get().typed_text.clone() {
            let terminal = self.terminal.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(3000), move || {
                terminal.feed_child(format!("{text}\n").as_bytes());
            });
        }
    }
}

impl ChildStatus {
    fn finish(pid: &Cell<i32>, status: i32) -> Self {
        pid.set(0);
        Self::from_wait(status)
    }

    fn from_wait(status: i32) -> Self {
        if libc::WIFEXITED(status) {
            Self::Exited(libc::WEXITSTATUS(status))
        } else if libc::WIFSIGNALED(status) {
            Self::Signaled(libc::WTERMSIG(status))
        } else {
            Self::Unknown(status)
        }
    }

    fn succeeded(self) -> bool {
        self == Self::Exited(0)
    }

    fn should_report(self, closing: bool) -> bool {
        !closing && !self.succeeded()
    }
}

impl std::fmt::Display for ChildStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exited(code) => write!(formatter, "exit code {code}"),
            Self::Signaled(signal) => write!(formatter, "signal {signal}"),
            Self::Unknown(status) => write!(formatter, "wait status {status:#x}"),
        }
    }
}

/// The state a pane is in between being drawn and its shell accepting a keystroke.
///
/// Nothing here changes launch control flow. It exists because a pane that has replayed history is
/// visually indistinguishable from a live shell, which is what let a Ctrl-C typed at a replayed
/// prompt look like a shell that had died.
///
/// The notice claimed for a while that "keystrokes are not delivered to a shell", and that was
/// false in the one direction a warning must never be false in. The pane owns its pty from
/// `PtyProcess::spawn` a few lines below, so a line typed against the replayed prompt is held in
/// the tty's input queue and read by the shell the instant it starts -- a verification run typed
/// one and watched it execute. A banner exists so a live pane can be told from a replayed one; a
/// banner the user can catch lying teaches them to distrust the true cases too. So it now says what
/// actually happens to the keystroke.
struct NotYetLive;

impl NotYetLive {
    /// Dim styling, matching the reopen notice, applied only where it is written to a terminal.
    const DIM: &'static str = "\u{1b}[2m";
    const RESET: &'static str = "\u{1b}[0m";

    /// What becomes of a keystroke typed while the pane is not live. True of both openings.
    const QUEUED: &'static str = "Anything you type now is queued by the terminal and runs when the shell starts.";

    fn notice(restoring: bool) -> String {
        let prefix = hl::runtime::domain::RESTORE_NOTICE_PREFIX;
        let words = if restoring {
            "Restoring this workspace. The output above is history from your last session; this pane is not live yet."
        } else {
            "Starting this workspace. This pane is not live yet."
        };
        let queued = Self::QUEUED;
        format!("\r\n{}{prefix}{words} {queued}{}\r\n", Self::DIM, Self::RESET)
    }
}

/// Builds a terminal for one persisted layout slot.
pub(crate) fn make_terminal_ex(
    tw: &Rc<TermWin>,
    cwd: Option<String>,
    history: Option<String>,
    slot: &str,
) -> (vte4::Terminal, Rc<Cell<i32>>) {
    let term = vte4::Terminal::new();
    let cfg = tw.ws.terminal_config();
    Terminal::new(&term).style(&cfg);
    term.set_font_scale(tw.zoom.scale());
    Terminal::new(&term).setup_hyperlinks();
    {
        let tw = tw.clone();
        let t = term.clone();
        let fc = gtk::EventControllerFocus::new();
        fc.connect_enter(move |_| {
            let previous = tw.focused.replace(Some(t.clone()));
            tw.copymode.focus(previous.clone(), &t);
            tw.search.focus(previous, t.clone());
        });
        term.add_controller(fc);
    }
    // Gentler, more natural scrolling. macOS trackpad / high-res wheel deltas make VTE's default scroll
    // fly by many lines per flick; intercept in the capture phase and move the scrollback a damped,
    // clamped number of lines. When there is no scrollback to move (alt-screen apps like htop/less/vim),
    // fall through so VTE still maps the wheel to arrow keys.
    {
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        let t = term.clone();
        scroll.connect_scroll(move |_, _dx, dy| {
            if let Some(adj) = t.vadjustment() {
                let max = adj.upper() - adj.page_size();
                if max <= adj.lower() {
                    return glib::Propagation::Proceed; // no scrollback (alt-screen) → let VTE handle it
                }
                let lines = (dy * 3.0).clamp(-5.0, 5.0);
                adj.set_value((adj.value() + lines).clamp(adj.lower(), max));
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        term.add_controller(scroll);
    }
    let pid = Rc::new(Cell::new(0));
    // Register this pane (terminal + its slot + pid) so the window's close handler can freeze it into its
    // own slot, and `save_session` can record which slot each pane owns.
    Slots::new(tw).hold(&term, slot.to_owned());
    let application = application_path().to_string_lossy().into_owned();
    let workspace_key = tw.ws.key();
    // The terminal always enters the workspace worker. A host-shell override here can make a workspace
    // look healthy while silently escaping the guest (for example, macOS `/bin/sh` reports `sh-3.2`).
    // Keep diagnostics explicit without changing the PTY contract.
    let dbg = AppConfig::get().debug_log.as_ref();
    let cwd_arg = cwd.filter(|c| c.starts_with('/'));
    let directory = cwd_arg.as_deref().unwrap_or("");
    let launch_args: Vec<&str> = vec![
        application.as_str(),
        "--worker",
        "launch",
        workspace_key.as_str(),
        slot,
        dbg.map_or("", String::as_str),
        directory,
    ];
    // A CLEAN minimal env — NOT the full parent env. Husklet runs under the nix devshell, whose
    // DYLD_*/GTK/GI library-path vars would poison `hl`'s dynamic loader (and its forked engine),
    // crashing it at startup (SIGSEGV). Pass only what a shell needs.
    let env = AppConfig::get().environment.terminal();
    let envv: Vec<&str> = env.iter().map(std::string::String::as_str).collect();
    // Replay saved scrollback/screen history (freeze/restore persistence) ABOVE the live shell, before
    // spawning, so the user's prior screen is visible the instant the window reopens.
    // Old sessions may contain launch diagnostics written before transient-output filtering was
    // introduced. Sanitize on read as well as write so a successful retry never appears to have
    // inherited the previous process's failure state.
    let replay = history
        .map(|text| HistorySnapshot::persistent(&text))
        .map(|history| session::History::new(&history).replay())
        .unwrap_or_default();
    if !replay.is_empty() {
        term.feed(&replay);
    }
    // Replayed scrollback ends in the prompt the previous session left behind, and that prompt does
    // not accept input: the worker below has not started a shell yet, and on a reopened workspace it
    // will not for as long as the restore takes. Say so, in the same attributed voice the restore
    // summary uses, so "restoring" is legible as a state distinct from "ready". The notice carries
    // the notice prefix, so it is filtered out of the scrollback this pane persists.
    term.feed(NotYetLive::notice(!replay.is_empty()).as_bytes());
    // NOTE: we deliberately do NOT use VTE's spawn_async — on macOS it fork()s inside the multithreaded
    // GTK process and does non-async-signal-safe work before exec, which crashes the child before it
    // runs (every command "exits 11"). Instead spawn via posix_spawn (async-safe) onto a PTY we own.
    match PtyProcess::spawn(&term, &launch_args, &envv) {
        Ok((child, pty)) => Launch {
            window: tw,
            terminal: &term,
            pid: &pid,
        }
        .attach(child, pty),
        Err(e) => term.feed(format!("\r\n\x1b[31mfailed to start shell: {e}\x1b[0m\r\n").as_bytes()),
    }
    (term, pid)
}

/// A URL matcher for auto-linking bare URLs (VTE turns matches into clickable regions). Explicit OSC-8
/// hyperlinks are handled separately (via `hyperlink_hover_uri`).
pub(crate) const URL_REGEX: &str = r"(?:https?://|www\.)[^\s<>\x22'`{}|\\^\[\]]+[^\s<>\x22'`{}|\\^\[\].,;:!?)]";

#[cfg(test)]
mod child_status_tests {
    use super::{ChildStatus, NotYetLive};

    #[test]
    fn a_pane_that_is_not_live_yet_says_so_in_husklets_own_attributed_voice() {
        let prefix = hl::runtime::domain::RESTORE_NOTICE_PREFIX;
        let restoring = NotYetLive::notice(true);
        let starting = NotYetLive::notice(false);

        for notice in [&restoring, &starting] {
            assert!(notice.contains(prefix), "the notice must be attributed to Husklet");
            assert!(
                notice.contains("not live yet"),
                "the notice must name the state, not merely decorate it"
            );
            // Persistence drops any line carrying the prefix, so the notice describes one launch
            // and can never be replayed as though it were guest output.
            assert!(
                super::HistorySnapshot::persistent(notice).trim().is_empty(),
                "the notice must never be persisted as scrollback"
            );
        }
        assert!(restoring.contains("history from your last session"));
        assert!(!starting.contains("history from your last session"));

        // The pane owns its pty before the notice can be read, so a keystroke is queued and run,
        // not dropped. Claiming otherwise is a false statement about the user's own input.
        for notice in [&restoring, &starting] {
            assert!(
                notice.contains("queued by the terminal and runs when the shell starts"),
                "the notice must say what becomes of a keystroke typed against it"
            );
            assert!(
                !notice.contains("not delivered"),
                "a keystroke typed at a not-yet-live pane is delivered; the notice must not deny it"
            );
        }
    }
    use std::cell::Cell;

    #[test]
    fn decodes_exit_and_signal_wait_statuses_without_inventing_255() {
        assert_eq!(ChildStatus::from_wait(0), ChildStatus::Exited(0));
        assert_eq!(ChildStatus::from_wait(7 << 8), ChildStatus::Exited(7));
        assert_eq!(
            ChildStatus::from_wait(libc::SIGTERM),
            ChildStatus::Signaled(libc::SIGTERM)
        );
        assert!(ChildStatus::from_wait(0).succeeded());
        assert!(!ChildStatus::from_wait(libc::SIGTERM).succeeded());
    }

    #[test]
    fn finishing_any_child_revokes_its_resize_and_signal_authority() {
        for status in [0, 7 << 8, libc::SIGTERM] {
            let pid = Cell::new(42);
            let _ = ChildStatus::finish(&pid, status);
            assert_eq!(pid.get(), 0);
        }
    }

    #[test]
    fn intentional_workspace_close_suppresses_launcher_exit_diagnostics() {
        for status in [
            ChildStatus::Signaled(libc::SIGHUP),
            ChildStatus::Signaled(libc::SIGKILL),
            ChildStatus::Exited(70),
        ] {
            assert!(!status.should_report(true));
            assert!(status.should_report(false));
        }
        assert!(!ChildStatus::Exited(0).should_report(false));
    }
}
