use super::*;

pub(crate) struct CloseRequest;

impl CloseRequest {
    pub(crate) fn install(window: &gtk::ApplicationWindow, terminal: &Rc<TermWin>) {
        let terminal = terminal.clone();
        let parent = window.clone();
        window.connect_close_request(move |_| {
            if terminal.closing.get() {
                return glib::Propagation::Proceed;
            }
            let terminal = terminal.clone();
            let parent = parent.clone();
            let dialog_parent = parent.clone();
            crate::gtk_adapter::Dialog::present(
                Some(dialog_parent.upcast_ref()),
                crate::components::dialog::CloseWorkspace::model(),
                move |event| {
                    let Some(choice) = crate::components::dialog::CloseWorkspace::choice(&event) else {
                        return;
                    };
                    Self::close(&parent, &terminal, choice);
                },
            );
            glib::Propagation::Stop
        });
    }

    fn close(parent: &gtk::ApplicationWindow, terminal: &Rc<TermWin>, choice: crate::components::dialog::CloseChoice) {
        terminal.closing.set(true);
        let preparation = match choice {
            crate::components::dialog::CloseChoice::Continue => WindowSession::new(terminal).save(),
            crate::components::dialog::CloseChoice::Kill => {
                Session::clear(&terminal.ws.storage_dir(&Home::current().root()))
            }
        };
        if let Err(error) = preparation {
            terminal.closing.set(false);
            Self::failure(parent, &error);
            return;
        }
        let result = std::sync::Arc::new(std::sync::Mutex::new(None));
        Self::spawn_close(terminal.ws.clone(), choice, &result);
        Self::poll_close(parent, terminal, result);
    }

    fn spawn_close(
        workspace: WorkspaceConfig,
        choice: crate::components::dialog::CloseChoice,
        result: &std::sync::Arc<std::sync::Mutex<Option<std::io::Result<()>>>>,
    ) {
        Self::spawn_close_with(workspace, choice, result, |workspace, disposition| {
            hl::runtime::domain::Domain::new(workspace)
                .close_handover(disposition, || Processes::close_workspace(&workspace.key()))
        });
    }

    fn spawn_close_with(
        workspace: WorkspaceConfig,
        choice: crate::components::dialog::CloseChoice,
        result: &std::sync::Arc<std::sync::Mutex<Option<std::io::Result<()>>>>,
        close_runtime: impl FnOnce(&WorkspaceConfig, hl::runtime::domain::Close) -> std::io::Result<()> + Send + 'static,
    ) {
        let completed = result.clone();
        std::thread::spawn(move || {
            let disposition = Self::disposition(choice);
            let closed = close_runtime(&workspace, disposition);
            if let Ok(mut result) = completed.lock() {
                *result = Some(closed);
            }
        });
    }

    fn disposition(choice: crate::components::dialog::CloseChoice) -> hl::runtime::domain::Close {
        match choice {
            crate::components::dialog::CloseChoice::Kill => hl::runtime::domain::Close::Kill,
            crate::components::dialog::CloseChoice::Continue => hl::runtime::domain::Close::Continue,
        }
    }

    fn poll_close(
        parent: &gtk::ApplicationWindow,
        terminal: &Rc<TermWin>,
        result: std::sync::Arc<std::sync::Mutex<Option<std::io::Result<()>>>>,
    ) {
        let terminal = terminal.clone();
        let parent = parent.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
            let completed = result.lock().ok().and_then(|mut value| value.take());
            let Some(completed) = completed else {
                return glib::ControlFlow::Continue;
            };
            Self::complete(&parent, &terminal, completed);
            glib::ControlFlow::Break
        });
    }

    fn complete(parent: &gtk::ApplicationWindow, terminal: &Rc<TermWin>, completed: std::io::Result<()>) {
        match completed {
            Ok(()) => parent.close(),
            Err(error) => {
                terminal.closing.set(false);
                Self::failure(parent, &error);
            }
        }
    }

    fn failure(parent: &gtk::ApplicationWindow, error: &std::io::Error) {
        // A close failure used to exist only inside this dialog, and the click that dismissed it
        // destroyed the only copy: `stderr` of the signed application goes nowhere, so a user
        // reporting "it errored" left no artifact at all. The engine's refusal reason arrives here
        // verbatim -- `close.result` carries it through `ResultFile::wait` -- so write that same
        // sentence down before showing it, and show it unchanged.
        hl_log::hl_error!(hl_log::tag::UI, "{}", Self::failure_line(error));
        crate::gtk_adapter::Dialog::present(Some(parent.upcast_ref()), Self::failure_dialog(error), |_| {});
    }

    /// The dialog the user sees, carrying the reason unabridged.
    fn failure_dialog(error: &std::io::Error) -> hl_gui::Dialog {
        hl_gui::Dialog::new("Could not close workspace")
            .detail(error.to_string())
            .action(hl_gui::Action::new(hl_gui::EventId::new("dismiss"), "Dismiss"))
    }

    /// The sentence written to the log. One line, so a multi-line engine refusal stays greppable.
    fn failure_line(error: &std::io::Error) -> String {
        format!(
            "could not close workspace: {}",
            error.to_string().replace('\n', " | ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{CloseRequest, Processes, WorkspaceConfig};
    use crate::components::dialog::CloseChoice;
    use std::io::BufRead as _;

    fn result() -> std::sync::Arc<std::sync::Mutex<Option<std::io::Result<()>>>> {
        std::sync::Arc::new(std::sync::Mutex::new(None))
    }

    fn wait(result: &std::sync::Arc<std::sync::Mutex<Option<std::io::Result<()>>>>) -> std::io::Result<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(result) = result.lock().unwrap().take() {
                return result;
            }
            assert!(std::time::Instant::now() < deadline, "workspace close thread timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    fn hup_resistant_launcher(workspace: &str) -> std::process::Child {
        use std::os::unix::process::CommandExt as _;

        // Use the stable system shell rather than executing a freshly-written fixture, which can race
        // the kernel's text-busy check under the default-parallel test harness. The shell's argument zero
        // still gives the process snapshot the exact production launcher identity.
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                "trap '' HUP TERM; printf 'ready\\n'; while :; do sleep 60; done",
                "husklet",
                "--worker",
                "launch",
                workspace,
                "test-pane",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        command.process_group(0);
        let mut child = command.spawn().unwrap();
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");
        child
    }

    #[cfg(unix)]
    fn assert_production_close_reaps(choice: CloseChoice) {
        let workspace = WorkspaceConfig::new(
            format!("close-reap-{}-{}", std::process::id(), choice as u8),
            "ubuntu",
            hl_ws::Arch::Arm64,
        );
        let key = workspace.key();
        let mut child = hup_resistant_launcher(&key);
        let completed = result();
        CloseRequest::spawn_close(workspace, choice, &completed);
        wait(&completed).unwrap();
        let status = child
            .try_wait()
            .unwrap()
            .expect("workspace close returned before its HUP-resistant launcher exited");
        assert!(!status.success());
    }

    #[cfg(unix)]
    #[test]
    fn kill_close_reaps_the_exact_workspace_launcher() {
        assert_production_close_reaps(CloseChoice::Kill);
    }

    #[cfg(unix)]
    #[test]
    fn continue_close_reaps_the_exact_workspace_launcher() {
        assert_production_close_reaps(CloseChoice::Continue);
    }

    #[cfg(unix)]
    #[test]
    fn domain_error_still_reaps_the_exact_workspace_launcher() {
        let workspace = WorkspaceConfig::new(
            format!("close-error-reap-{}", std::process::id()),
            "ubuntu",
            hl_ws::Arch::Arm64,
        );
        let key = workspace.key();
        let mut child = hup_resistant_launcher(&key);
        let completed = result();
        CloseRequest::spawn_close_with(workspace, CloseChoice::Kill, &completed, move |_, disposition| {
            assert_eq!(disposition, hl::runtime::domain::Close::Kill);
            Processes::close_workspace(&key)?;
            Err(std::io::Error::other("domain close failed"))
        });
        assert_eq!(wait(&completed).unwrap_err().to_string(), "domain close failed");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while child.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if child.try_wait().unwrap().is_none() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("domain failure stranded its HUP-resistant launcher");
        }
    }

    /// The Continue mirror of [`domain_error_still_reaps_the_exact_workspace_launcher`].
    ///
    /// A capture that reports failure is the observed production case -- `close.result` carrying
    /// `runtime failed: ...` -- and the daemon stops on the shutdown request regardless of what the
    /// capture reported. The launcher workers must therefore be reaped on this path too, and the
    /// close must still surface the failure to the user.
    #[cfg(unix)]
    #[test]
    fn continue_error_still_reaps_the_exact_workspace_launcher() {
        let workspace = WorkspaceConfig::new(
            format!("close-continue-error-reap-{}", std::process::id()),
            "ubuntu",
            hl_ws::Arch::Arm64,
        );
        let key = workspace.key();
        let mut child = hup_resistant_launcher(&key);
        let completed = result();
        CloseRequest::spawn_close_with(workspace, CloseChoice::Continue, &completed, move |_, disposition| {
            assert_eq!(disposition, hl::runtime::domain::Close::Continue);
            Processes::close_workspace(&key)?;
            Err(std::io::Error::other("runtime failed: capture timed out"))
        });
        assert_eq!(
            wait(&completed).unwrap_err().to_string(),
            "runtime failed: capture timed out"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while child.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if child.try_wait().unwrap().is_none() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("a failed continue stranded its HUP-resistant launcher");
        }
    }

    /// The whole point of the refusal reasons the engine now produces is that the user reads one.
    /// `ResultFile::wait` turns `close.result` into exactly this error, so a generic sentence here
    /// would discard a reason the system already knew.
    #[test]
    fn the_engine_refusal_reason_reaches_the_dialog_the_user_sees() {
        let refused = std::io::Error::other("checkpoint refused by the engine: guest fd 10 is a pipe");
        let dialog = CloseRequest::failure_dialog(&refused);

        assert_eq!(dialog.title, "Could not close workspace");
        assert_eq!(
            dialog.detail.as_deref(),
            Some("checkpoint refused by the engine: guest fd 10 is a pipe")
        );
    }

    /// The same reason must survive the click that dismissed the dialog.
    #[test]
    fn a_close_failure_is_written_as_one_greppable_line() {
        let refused = std::io::Error::other("1 of 7 participants never committed\nexecution 7f3a: no reply");

        assert_eq!(
            CloseRequest::failure_line(&refused),
            "could not close workspace: 1 of 7 participants never committed | execution 7f3a: no reply"
        );
    }

    #[test]
    fn close_choices_map_to_their_domain_dispositions() {
        assert_eq!(
            CloseRequest::disposition(CloseChoice::Kill),
            hl::runtime::domain::Close::Kill
        );
        assert_eq!(
            CloseRequest::disposition(CloseChoice::Continue),
            hl::runtime::domain::Close::Continue
        );
    }
}
