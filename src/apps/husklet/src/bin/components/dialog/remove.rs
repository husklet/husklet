use hl_gui::{Action, Dialog, EventId};

pub(crate) struct RemoveWorkspace {
    name: String,
}

impl RemoveWorkspace {
    pub(crate) fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    pub(crate) fn present(
        self,
        parent: Option<&gtk::Window>,
        on_remove: impl Fn() -> std::io::Result<()> + 'static,
    ) {
        let remove = EventId::new("remove");
        let model = Dialog::new(format!("Remove {}?", self.name))
            .detail("Its files on disk are kept. Only the workspace entry is removed.")
            .action(Action::new(EventId::new("cancel"), "Cancel").suggested())
            .action(Action::new(remove.clone(), "Remove").destructive());

        let parent = parent.cloned();
        let confirmation_parent = parent.clone();
        hl_gui::gtk::Dialog::present(confirmation_parent.as_ref(), model, move |event| {
            if event == remove {
                if let Err(error) = on_remove() {
                    let dismiss = EventId::new("dismiss");
                    let failure = Dialog::new("Could not remove workspace")
                        .detail(error.to_string())
                        .action(Action::new(dismiss, "OK").suggested());
                    hl_gui::gtk::Dialog::present(parent.as_ref(), failure, |_| {});
                }
            }
        });
    }
}
