use hl_gui::{Action, Dialog, EventId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Choice {
    Kill,
    Continue,
}

pub(crate) struct CloseWorkspace;

impl CloseWorkspace {
    pub(crate) fn model(checkpoint_available: bool) -> Dialog {
        let dialog = Dialog::new("Close workspace?")
            .detail(if checkpoint_available {
                "Continue later restores running commands, tabs, panes, and history. Kill stops everything without a checkpoint."
            } else {
                "This workspace uses live-only sessions. They can reconnect while the workspace is running, but cannot continue after shutdown. Cancel to keep them running, or kill them now."
            })
            .action(Action::new(Self::cancel(), "Cancel").suggested())
            .action(Action::new(Self::kill(), "Kill workspace").destructive());
        if checkpoint_available {
            dialog.action(Action::new(Self::later(), "Continue later"))
        } else {
            dialog
        }
    }

    pub(crate) fn choice(event: &EventId) -> Option<Choice> {
        if event == &Self::kill() {
            Some(Choice::Kill)
        } else if event == &Self::later() {
            Some(Choice::Continue)
        } else {
            None
        }
    }

    fn kill() -> EventId {
        EventId::new("kill-workspace")
    }

    fn cancel() -> EventId {
        EventId::new("cancel-close-workspace")
    }

    fn later() -> EventId {
        EventId::new("continue-workspace-later")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_dialog_defaults_to_an_explicit_non_closing_choice() {
        let model = CloseWorkspace::model(true);
        assert_eq!(model.actions.len(), 3);
        assert_eq!(model.actions[0].label, "Cancel");
        assert_eq!(CloseWorkspace::choice(&model.actions[0].id), None);
        assert_eq!(model.actions[0].role, hl_gui::Role::Suggested);
        assert_eq!(CloseWorkspace::choice(&model.actions[1].id), Some(Choice::Kill));
        assert_eq!(model.actions[1].role, hl_gui::Role::Destructive);
        assert_eq!(CloseWorkspace::choice(&model.actions[2].id), Some(Choice::Continue));
        assert_eq!(model.actions[2].role, hl_gui::Role::Default);
    }

    #[test]
    fn live_only_close_dialog_cannot_request_a_checkpoint() {
        let model = CloseWorkspace::model(false);
        assert_eq!(model.actions.len(), 2);
        assert!(
            model
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("cannot continue after shutdown"))
        );
        assert!(
            model
                .actions
                .iter()
                .all(|action| CloseWorkspace::choice(&action.id) != Some(Choice::Continue))
        );
    }
}
