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
        Self::spawn_close(terminal, choice, &result);
        Self::poll_close(parent, terminal, result);
    }

    fn spawn_close(
        terminal: &Rc<TermWin>,
        choice: crate::components::dialog::CloseChoice,
        result: &std::sync::Arc<std::sync::Mutex<Option<std::io::Result<()>>>>,
    ) {
        let completed = result.clone();
        let workspace = terminal.ws.clone();
        std::thread::spawn(move || {
            let disposition = match choice {
                crate::components::dialog::CloseChoice::Kill => hl::runtime::domain::Close::Kill,
                crate::components::dialog::CloseChoice::Continue => hl::runtime::domain::Close::Continue,
            };
            let closed = Self::close_runtime(
                || hl::runtime::domain::Domain::new(&workspace).close(disposition),
                || Processes::close_workspace(&workspace.key()),
            );
            if let Ok(mut result) = completed.lock() {
                *result = Some(closed);
            }
        });
    }

    fn close_runtime(
        close_domain: impl FnOnce() -> std::io::Result<()>,
        close_launchers: impl FnOnce() -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let domain = close_domain();
        let launchers = close_launchers();
        domain.and(launchers)
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
        let failure = hl_gui::Dialog::new("Could not close workspace")
            .detail(error.to_string())
            .action(hl_gui::Action::new(hl_gui::EventId::new("dismiss"), "Dismiss"));
        crate::gtk_adapter::Dialog::present(Some(parent.upcast_ref()), failure, |_| {});
    }
}

#[cfg(test)]
mod tests {
    use super::CloseRequest;
    use std::cell::Cell;

    #[test]
    fn workspace_close_reaps_launchers_after_the_domain() {
        let stage = Cell::new(0);
        CloseRequest::close_runtime(
            || {
                assert_eq!(stage.replace(1), 0);
                Ok(())
            },
            || {
                assert_eq!(stage.replace(2), 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(stage.get(), 2);
    }

    #[test]
    fn failed_domain_close_still_reaps_launchers() {
        let reaped = Cell::new(false);
        let error = CloseRequest::close_runtime(
            || Err(std::io::Error::other("domain close failed")),
            || {
                reaped.set(true);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(reaped.get());
        assert_eq!(error.to_string(), "domain close failed");
    }
}
