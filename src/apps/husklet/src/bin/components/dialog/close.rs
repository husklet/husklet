use hl_gui::{Action, Dialog, EventId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Choice {
    Kill,
    Continue,
}

pub(crate) struct CloseWorkspace;

impl CloseWorkspace {
    pub(crate) fn model() -> Dialog {
        Dialog::new("Close workspace?")
            .detail(
                "Continue later restores running commands, tabs, panes, and history. Kill stops everything without a checkpoint.",
            )
            .action(Action::new(Self::kill(), "Kill workspace").destructive())
            .action(Action::new(Self::later(), "Continue later").suggested())
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

    fn later() -> EventId {
        EventId::new("continue-workspace-later")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_dialog_exposes_explicit_destructive_and_restorable_choices() {
        let model = CloseWorkspace::model();
        assert_eq!(model.actions.len(), 2);
        assert_eq!(
            CloseWorkspace::choice(&model.actions[0].id),
            Some(Choice::Kill)
        );
        assert_eq!(
            CloseWorkspace::choice(&model.actions[1].id),
            Some(Choice::Continue)
        );
        assert_eq!(model.actions[0].role, hl_gui::Role::Destructive);
        assert_eq!(model.actions[1].role, hl_gui::Role::Suggested);
    }
}
