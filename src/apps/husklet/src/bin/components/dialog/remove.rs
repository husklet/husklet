use hl_gui::{Action, Dialog, EventId};

pub(crate) struct RemoveWorkspace {
    name: String,
}

impl RemoveWorkspace {
    pub(crate) fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    pub(crate) fn present(self, parent: Option<&gtk::Window>, on_remove: impl Fn() + 'static) {
        let remove = EventId::new("remove");
        let model = Dialog::new(format!("Remove {}?", self.name))
            .detail("Its files on disk are kept. Only the workspace entry is removed.")
            .action(Action::new(EventId::new("cancel"), "Cancel"))
            .action(Action::new(remove.clone(), "Remove").destructive());

        hl_gui::gtk::Dialog::present(parent, model, move |event| {
            if event == remove {
                on_remove();
            }
        });
    }
}
