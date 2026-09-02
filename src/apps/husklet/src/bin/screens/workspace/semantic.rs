//! Semantic ownership for Husklet's native workspace pages.
//!
//! This is deliberately populated by the screen that owns an action.  It does
//! not walk GTK's widget tree, infer labels, or manufacture coordinates.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

pub const NODE_LIMIT: usize = 256;
pub const DEPTH_LIMIT: usize = 32;
pub const TEXT_LIMIT: usize = 256;
pub const ACTION_VALUE_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionKind {
    Invoke,
    Change,
    Submit,
    Toggle,
    Expand,
    Focus,
}

/// A value's disclosure policy. Secret contents never enter registry storage.
pub enum Value<'a> {
    Public(&'a str),
    Secret,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node {
    pub id: u64,
    pub role: String,
    pub label: Option<String>,
    pub value: Option<String>,
    pub disabled: bool,
    pub destructive: bool,
    pub actions: Vec<ActionKind>,
    pub children: Vec<Node>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub slot: String,
    pub revision: u64,
    pub root: Node,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Action {
    pub revision: u64,
    pub node: u64,
    pub action: ActionKind,
    pub value: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    Stale { current: u64 },
    Absent(u64),
    Unsupported(ActionKind),
    Disabled(u64),
    ValueTooLong,
}

struct Entry {
    path: String,
    node: Node,
    act: Rc<dyn Fn(ActionKind, Option<&str>)>,
}

/// A bounded, revisioned registry for one native pane.
#[derive(Clone)]
pub struct Registry {
    slot: String,
    revision: Rc<Cell<u64>>,
    entries: Rc<RefCell<BTreeMap<u64, Entry>>>,
}

impl Registry {
    #[must_use]
    pub fn new(slot: impl Into<String>) -> Self {
        Self {
            slot: slot.into(),
            revision: Rc::new(Cell::new(1)),
            entries: Rc::new(RefCell::new(BTreeMap::new())),
        }
    }

    /// Registers one element under a product-owned stable path.
    pub fn register(
        &self,
        path: &str,
        role: &str,
        label: Option<&str>,
        value: Option<Value<'_>>,
        actions: &[ActionKind],
        act: Rc<dyn Fn(ActionKind, Option<&str>)>,
    ) -> u64 {
        let id = stable_id(path);
        let node = Node {
            id,
            role: bounded(role),
            label: label.map(bounded),
            value: value.map(|value| match value {
                Value::Public(value) => bounded(value),
                Value::Secret => "[redacted]".to_owned(),
            }),
            disabled: false,
            destructive: false,
            actions: actions.to_vec(),
            children: Vec::new(),
        };
        self.entries.borrow_mut().insert(
            id,
            Entry {
                path: path.to_owned(),
                node,
                act,
            },
        );
        self.bump();
        id
    }

    pub fn remove(&self, path: &str) {
        if self.entries.borrow_mut().remove(&stable_id(path)).is_some() {
            self.bump();
        }
    }

    pub fn remove_prefix(&self, prefix: &str) {
        let before = self.entries.borrow().len();
        self.entries
            .borrow_mut()
            .retain(|_, entry| !entry.path.starts_with(prefix));
        if self.entries.borrow().len() != before {
            self.bump();
        }
    }

    pub fn update(&self, path: &str, value: Value<'_>, disabled: bool) {
        let mut entries = self.entries.borrow_mut();
        let Some(entry) = entries.get_mut(&stable_id(path)) else {
            return;
        };
        let value = match value {
            Value::Public(value) => bounded(value),
            Value::Secret => "[redacted]".to_owned(),
        };
        if entry.node.value.as_deref() != Some(&value) || entry.node.disabled != disabled {
            entry.node.value = Some(value);
            entry.node.disabled = disabled;
            self.bump();
        }
    }

    pub fn set_disabled(&self, path: &str, disabled: bool) {
        let mut entries = self.entries.borrow_mut();
        let Some(entry) = entries.get_mut(&stable_id(path)) else {
            return;
        };
        if entry.node.disabled != disabled {
            entry.node.disabled = disabled;
            self.bump();
        }
    }

    pub fn set_destructive(&self, path: &str) {
        let mut entries = self.entries.borrow_mut();
        let Some(entry) = entries.get_mut(&stable_id(path)) else {
            return;
        };
        if !entry.node.destructive {
            entry.node.destructive = true;
            self.bump();
        }
    }

    pub fn requirement(&self, node: u64) -> Result<hl_extension::Capability, Refusal> {
        let entries = self.entries.borrow();
        let entry = entries.get(&node).ok_or(Refusal::Absent(node))?;
        if entry.path.starts_with("settings/") {
            Ok(hl_extension::Capability::WorkspaceControl)
        } else if entry.path.starts_with("extensions/") {
            Ok(hl_extension::Capability::ExtensionControl)
        } else {
            Ok(hl_extension::Capability::PaneSemanticControl)
        }
    }

    pub fn select(&self, path: &str) {
        let selected = stable_id(path);
        let mut changed = false;
        for entry in self.entries.borrow_mut().values_mut() {
            if entry.node.role != "tab" {
                continue;
            }
            let next = (entry.node.id == selected).to_string();
            if entry.node.value.as_deref() != Some(&next) {
                entry.node.value = Some(next);
                changed = true;
            }
        }
        if changed {
            self.bump();
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let mut children = Vec::new();
        let mut truncated = false;
        for entry in self.entries.borrow().values() {
            if children.len() + 1 >= NODE_LIMIT {
                truncated = true;
                break;
            }
            let mut node = entry.node.clone();
            if node.disabled {
                node.actions.clear();
            }
            children.push(node);
        }
        Snapshot {
            slot: self.slot.clone(),
            revision: self.revision.get(),
            root: Node {
                id: stable_id(&format!("{}/root", self.slot)),
                role: "navigation".to_owned(),
                label: Some("Workspace".to_owned()),
                value: None,
                disabled: false,
                destructive: false,
                actions: Vec::new(),
                children,
            },
            truncated,
        }
    }

    pub fn act(&self, action: &Action) -> Result<(), Refusal> {
        let current = self.revision.get();
        if action.revision != current {
            return Err(Refusal::Stale { current });
        }
        if action
            .value
            .as_ref()
            .is_some_and(|value| value.len() > ACTION_VALUE_LIMIT)
        {
            return Err(Refusal::ValueTooLong);
        }
        let entries = self.entries.borrow();
        let entry = entries.get(&action.node).ok_or(Refusal::Absent(action.node))?;
        if entry.node.disabled {
            return Err(Refusal::Disabled(action.node));
        }
        if !entry.node.actions.contains(&action.action) {
            return Err(Refusal::Unsupported(action.action));
        }
        let callback = Rc::clone(&entry.act);
        drop(entries);
        callback(action.action, action.value.as_deref());
        Ok(())
    }

    fn bump(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }
}

fn bounded(value: &str) -> String {
    value.chars().take(TEXT_LIMIT).collect()
}

/// FNV-1a over a durable product path. IDs do not depend on widget allocation.
#[must_use]
pub const fn stable_id(path: &str) -> u64 {
    let bytes = path.as_bytes();
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut index = 0;
    while index < bytes.len() {
        hash ^= bytes[index] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        index += 1;
    }
    // The extension SDK and MCP transport carry semantic IDs as JavaScript
    // numbers. Keep product-owned hashes exactly representable across that
    // boundary rather than letting a round trip silently address a neighbour.
    hash & 0x001f_ffff_ffff_ffff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_are_bounded_and_actions_require_the_observed_revision() {
        let registry = Registry::new("workspace");
        let invoked = Rc::new(Cell::new(false));
        let mark = Rc::clone(&invoked);
        let id = registry.register(
            "workspace/settings",
            "tab",
            Some(&"s".repeat(TEXT_LIMIT + 10)),
            Some(Value::Secret),
            &[ActionKind::Invoke],
            Rc::new(move |_, _| mark.set(true)),
        );
        let first = registry.snapshot();
        assert_eq!(
            first.root.children[0].label.as_ref().unwrap().chars().count(),
            TEXT_LIMIT
        );
        assert_eq!(first.root.children[0].value.as_deref(), Some("[redacted]"));
        registry.register(
            "workspace/extensions",
            "tab",
            Some("Extensions"),
            None,
            &[],
            Rc::new(|_, _| {}),
        );
        assert_eq!(
            registry.act(&Action {
                revision: first.revision,
                node: id,
                action: ActionKind::Invoke,
                value: None
            }),
            Err(Refusal::Stale {
                current: registry.snapshot().revision
            })
        );
        assert!(!invoked.get());
        registry
            .act(&Action {
                revision: registry.snapshot().revision,
                node: id,
                action: ActionKind::Invoke,
                value: None,
            })
            .unwrap();
        assert!(invoked.get());
    }

    #[test]
    fn stable_ids_are_allocation_and_order_independent() {
        assert_eq!(stable_id("workspace/settings"), stable_id("workspace/settings"));
        assert_ne!(stable_id("workspace/settings"), stable_id("workspace/extensions"));
        assert!(stable_id("workspace/settings") <= 9_007_199_254_740_991);
    }

    #[test]
    fn snapshots_and_action_values_have_hard_limits() {
        let registry = Registry::new("workspace");
        for index in 0..(NODE_LIMIT + 10) {
            registry.register(
                &format!("workspace/page/{index}"),
                "tab",
                None,
                None,
                &[ActionKind::Change],
                Rc::new(|_, _| {}),
            );
        }
        let snapshot = registry.snapshot();
        assert!(snapshot.truncated);
        assert_eq!(snapshot.root.children.len(), NODE_LIMIT - 1);
        assert_eq!(
            registry.act(&Action {
                revision: snapshot.revision,
                node: snapshot.root.children[0].id,
                action: ActionKind::Change,
                value: Some("x".repeat(ACTION_VALUE_LIMIT + 1)),
            }),
            Err(Refusal::ValueTooLong)
        );
    }

    #[test]
    fn destructive_confirmation_is_disabled_until_phase_one_and_changes_revision() {
        let registry = Registry::new("workspace");
        let removed = Rc::new(Cell::new(false));
        let mark = Rc::clone(&removed);
        let path = "extensions/installed/demo/Confirm removal";
        let id = registry.register(
            path,
            "button",
            Some("Confirm removal"),
            None,
            &[ActionKind::Invoke],
            Rc::new(move |_, _| mark.set(true)),
        );
        registry.set_destructive(path);
        registry.set_disabled(path, true);
        let initial = registry.snapshot();
        let node = initial
            .root
            .children
            .iter()
            .find(|node| node.id == id)
            .expect("confirm node");
        assert!(node.destructive);
        assert!(node.disabled);
        assert_eq!(
            registry.act(&Action {
                revision: initial.revision,
                node: id,
                action: ActionKind::Invoke,
                value: None,
            }),
            Err(Refusal::Disabled(id))
        );
        assert!(!removed.get(), "initial direct confirmation preserved the installation");
        registry.set_disabled(path, false);
        let armed = registry.snapshot();
        assert_ne!(
            armed.revision, initial.revision,
            "phase transition must stale the initial snapshot"
        );
        assert!(matches!(
            registry.act(&Action {
                revision: initial.revision,
                node: id,
                action: ActionKind::Invoke,
                value: None,
            }),
            Err(Refusal::Stale { .. })
        ));
        registry
            .act(&Action {
                revision: armed.revision,
                node: id,
                action: ActionKind::Invoke,
                value: None,
            })
            .expect("armed confirmation");
        assert!(removed.get());
    }

    #[test]
    fn native_action_paths_resolve_to_their_domain_authority() {
        let registry = Registry::new("workspace");
        let settings = registry.register(
            "settings/save",
            "button",
            None,
            None,
            &[ActionKind::Invoke],
            Rc::new(|_, _| {}),
        );
        let extension = registry.register(
            "extensions/installed/demo/Disable",
            "button",
            None,
            None,
            &[ActionKind::Invoke],
            Rc::new(|_, _| {}),
        );
        assert_eq!(
            registry.requirement(settings),
            Ok(hl_extension::Capability::WorkspaceControl)
        );
        assert_eq!(
            registry.requirement(extension),
            Ok(hl_extension::Capability::ExtensionControl)
        );
    }
}
