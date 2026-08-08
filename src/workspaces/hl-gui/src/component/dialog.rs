use super::EventId;

/// Toolkit-neutral state for a modal dialog with an ordered set of actions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dialog {
    pub title: String,
    pub detail: Option<String>,
    pub actions: Vec<Action>,
}

impl Dialog {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            detail: None,
            actions: Vec::new(),
        }
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn action(mut self, action: Action) -> Self {
        self.actions.push(action);
        self
    }
}

/// One user choice emitted by a dialog.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Action {
    pub id: EventId,
    pub label: String,
    pub role: Role,
}

impl Action {
    pub fn new(id: EventId, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            role: Role::Default,
        }
    }

    #[must_use]
    pub fn suggested(mut self) -> Self {
        self.role = Role::Suggested;
        self
    }

    #[must_use]
    pub fn destructive(mut self) -> Self {
        self.role = Role::Destructive;
        self
    }
}

/// Presentation semantics for an action; toolkit adapters choose the concrete style.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Role {
    #[default]
    Default,
    Suggested,
    Destructive,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_preserves_content_and_action_order() {
        let cancel = EventId::new("cancel");
        let confirm = EventId::new("confirm");
        let dialog = Dialog::new("Remove item?")
            .detail("This cannot be undone.")
            .action(Action::new(cancel.clone(), "Cancel"))
            .action(Action::new(confirm.clone(), "Remove").destructive());

        assert_eq!(dialog.title, "Remove item?");
        assert_eq!(dialog.detail.as_deref(), Some("This cannot be undone."));
        assert_eq!(dialog.actions[0].id, cancel);
        assert_eq!(dialog.actions[0].role, Role::Default);
        assert_eq!(dialog.actions[1].id, confirm);
        assert_eq!(dialog.actions[1].role, Role::Destructive);
    }

    #[test]
    fn suggested_action_keeps_typed_identity() {
        let id = EventId::new("continue");
        let action = Action::new(id.clone(), "Continue").suggested();
        assert_eq!(action.id, id);
        assert_eq!(action.role, Role::Suggested);
    }
}
