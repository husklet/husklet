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
            if !status.succeeded() {
                terminal.feed(format!("\r\n\x1b[31mworkspace session ended ({status})\x1b[0m\r\n").as_bytes());
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
    tw.panes
        .borrow_mut()
        .push(PaneRegistration::new(&term, slot.to_owned()));
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
    use super::ChildStatus;
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
}
