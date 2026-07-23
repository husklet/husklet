use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChildStatus {
    Exited(i32),
    Signaled(i32),
    Unknown(i32),
}

impl ChildStatus {
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
    slot: String,
) -> (vte4::Terminal, Rc<Cell<i32>>) {
    let term = vte4::Terminal::new();
    let cfg = tw.ws.terminal_config();
    Terminal::new(&term).style(&cfg);
    Terminal::new(&term).setup_hyperlinks();
    {
        let tw = tw.clone();
        let t = term.clone();
        let fc = gtk::EventControllerFocus::new();
        fc.connect_enter(move |_| *tw.focused.borrow_mut() = Some(t.clone()));
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
        .push(PaneRegistration::new(&term, slot.clone(), pid.clone()));
    let application = application_path().to_string_lossy().into_owned();
    let workspace_key = tw.ws.key();
    // DEBUG: HL_TERM_CMD overrides the whole command (isolate VTE-spawn vs hl). The debug-log path is
    // passed to the worker explicitly: redirecting its standard streams would change the PTY contract being
    // diagnosed.
    let testcmd = AppConfig::get().command.as_ref();
    let dbg = AppConfig::get().debug_log.as_ref();
    let cwd_arg = cwd.filter(|c| c.starts_with('/'));
    let directory = cwd_arg.as_deref().unwrap_or("");
    let launch_args: Vec<&str> = vec![
        application.as_str(),
        "--worker",
        "launch",
        workspace_key.as_str(),
        slot.as_str(),
        dbg.map(String::as_str).unwrap_or(""),
        directory,
    ];
    let argv: Vec<&str> = if let Some(c) = &testcmd {
        vec!["/bin/sh", "-c", c.as_str()]
    } else {
        launch_args
    };
    // A CLEAN minimal env — NOT the full parent env. Husklet runs under the nix devshell, whose
    // DYLD_*/GTK/GI library-path vars would poison `hl`'s dynamic loader (and its forked engine),
    // crashing it at startup (SIGSEGV). Pass only what a shell needs.
    let env = AppConfig::get().environment.terminal();
    let envv: Vec<&str> = env.iter().map(|s| s.as_str()).collect();
    // Replay saved scrollback/screen history (freeze/restore persistence) ABOVE the live shell, before
    // spawning, so the user's prior screen is visible the instant the window reopens.
    if let Some(text) = history {
        // Old sessions may contain launch diagnostics written before transient-output filtering was
        // introduced. Sanitize on read as well as write so a successful retry never appears to have
        // inherited the previous process's failure state.
        let history = HistorySnapshot::persistent(&text);
        let bytes = session::History::new(&history).replay();
        if !bytes.is_empty() {
            term.feed(&bytes);
        }
    }
    // NOTE: we deliberately do NOT use VTE's spawn_async — on macOS it fork()s inside the multithreaded
    // GTK process and does non-async-signal-safe work before exec, which crashes the child before it
    // runs (every command "exits 11"). Instead spawn via posix_spawn (async-safe) onto a PTY we own.
    match PtyProcess::spawn(&term, &argv, &envv) {
        Ok((child, pty)) => {
            pid.set(child);
            // Keep the FOREIGN pty sized to the terminal grid — VTE doesn't resize a foreign pty itself,
            // so without this htop is malformed / half-height and doesn't reflow on window resize.
            let weak = term.downgrade();
            let mut last = (0, 0);
            glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
                let Some(t) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                let (c, r) = (t.column_count() as i32, t.row_count() as i32);
                if c > 0 && r > 0 && (c, r) != last {
                    let _ = pty.set_size(r, c);
                    last = (c, r);
                }
                glib::ControlFlow::Continue
            });
            // Shell exit → close this pane/tab (collapse a split, else close the tab). BUT a shell that dies
            // almost immediately means the LAUNCH failed (e.g. the host was momentarily saturated and the
            // engine couldn't start) — don't silently vanish the tab, which reads as "the shortcut did
            // nothing". Show the exit inline and keep the pane so the failure is visible and retryable.
            let tw2 = tw.clone();
            let te = term.clone();
            let born = std::time::Instant::now();
            glib::child_watch_add_local(glib::Pid(child), move |_pid, status| {
                let status = ChildStatus::from_wait(status);
                if !status.succeeded() && born.elapsed() < std::time::Duration::from_millis(2500) {
                    te.feed(
                        format!(
                            "\r\n\x1b[31mworkspace session ended immediately ({status})\x1b[0m\r\n"
                        )
                        .as_bytes(),
                    );
                    return;
                }
                TerminalPane::new(&tw2, &te).close();
            });
            if let Some(text) = AppConfig::get().typed_text.clone() {
                let t2 = term.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(3000), move || {
                    t2.feed_child(format!("{text}\n").as_bytes());
                });
            }
        }
        Err(e) => {
            term.feed(format!("\r\n\x1b[31mfailed to start shell: {e}\x1b[0m\r\n").as_bytes())
        }
    }
    (term, pid)
}

/// A URL matcher for auto-linking bare URLs (VTE turns matches into clickable regions). Explicit OSC-8
/// hyperlinks are handled separately (via `hyperlink_hover_uri`).
pub(crate) const URL_REGEX: &str =
    r"(?:https?://|www\.)[^\s<>\x22'`{}|\\^\[\]]+[^\s<>\x22'`{}|\\^\[\].,;:!?)]";

#[cfg(test)]
mod child_status_tests {
    use super::ChildStatus;

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
}
