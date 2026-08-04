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
                    terminal.closing.set(true);
                    let preparation = match choice {
                        crate::components::dialog::CloseChoice::Continue => WindowSession::new(&terminal).save(),
                        crate::components::dialog::CloseChoice::Kill => {
                            Session::clear(&terminal.ws.storage_dir(&Home::current().root()))
                        }
                    };
                    if let Err(error) = preparation {
                        terminal.closing.set(false);
                        Self::failure(&parent, error);
                        return;
                    }

                    let result = std::sync::Arc::new(std::sync::Mutex::new(None));
                    let completed = result.clone();
                    let workspace = terminal.ws.clone();
                    std::thread::spawn(move || {
                        let disposition = match choice {
                            crate::components::dialog::CloseChoice::Kill => hl::runtime::domain::Close::Kill,
                            crate::components::dialog::CloseChoice::Continue => hl::runtime::domain::Close::Continue,
                        };
                        let closed = hl::runtime::domain::Domain::new(&workspace).close(disposition);
                        if let Ok(mut result) = completed.lock() {
                            *result = Some(closed);
                        }
                    });

                    let terminal = terminal.clone();
                    let parent = parent.clone();
                    glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
                        let completed = result.lock().ok().and_then(|mut value| value.take());
                        let Some(completed) = completed else {
                            return glib::ControlFlow::Continue;
                        };
                        match completed {
                            Ok(()) => parent.close(),
                            Err(error) => {
                                terminal.closing.set(false);
                                Self::failure(&parent, error);
                            }
                        }
                        glib::ControlFlow::Break
                    });
                },
            );
            glib::Propagation::Stop
        });
    }

    fn failure(parent: &gtk::ApplicationWindow, error: std::io::Error) {
        let failure = hl_gui::Dialog::new("Could not close workspace")
            .detail(error.to_string())
            .action(hl_gui::Action::new(hl_gui::EventId::new("dismiss"), "Dismiss"));
        crate::gtk_adapter::Dialog::present(Some(parent.upcast_ref()), failure, |_| {});
    }
}
