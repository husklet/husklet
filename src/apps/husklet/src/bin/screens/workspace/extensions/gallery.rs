//! Where each extension's interface widget is, so a pane can hold the one that
//! already exists.
//!
//! An extension draws one interface: a stream of reconciliation frames applied
//! to one tree. When an extension asks for a pane to draw into, the interface
//! it is already drawing is what goes in the pane, and its page on the
//! workspace shell keeps the empty holder it came out of until it comes back.
//! Anything else would be a second tree that never received the frames the
//! first was built from.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

/// One extension's interface and the page it belongs to.
struct Exhibit {
    generation: u64,
    /// The widget the extension's frames are applied to.
    interface: glib::WeakRef<gtk::Widget>,
    /// The holder on the workspace shell it was placed in, which is where it
    /// goes back to when a pane holding it closes.
    home: glib::WeakRef<gtk::Box>,
    providers: Vec<hl_extension::PaneProvider>,
    /// Provider authority begins only after this generation has reconciled one
    /// valid frame. A persisted/enabled record is not proof that its sidecar is
    /// ready to draw an interface.
    ready: bool,
    selected: Rc<dyn Fn(hl_extension::PaneSelection)>,
    semantics: Option<Rc<dyn Fn(&str) -> Result<hl_extension::PaneSemanticTree, hl_extension::HostError>>>,
    action: Option<Rc<dyn Fn(&str, &hl_extension::PaneSemanticAction) -> Result<(), hl_extension::HostError>>>,
    pane: Option<Rc<dyn Fn(&str) -> gtk::Widget>>,
    retire: Option<Rc<dyn Fn(&str)>>,
    shutdown: Option<Rc<dyn Fn()>>,
}

/// One choice shown by a terminal pane, tied to the extension that owns it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Provider {
    pub extension: String,
    /// The exact live enrolment that published this choice.
    pub generation: u64,
    pub id: String,
    pub title: String,
    pub icon: Option<String>,
}

/// Every extension's interface, by name.
///
/// Held weakly throughout: a shell that closed takes its pages with it, and a
/// gallery is a place to look something up, not a reason for a widget to
/// outlive the window it was drawn in.
#[derive(Clone, Default)]
pub struct Gallery(
    Rc<RefCell<HashMap<String, Exhibit>>>,
    Rc<RefCell<Option<super::super::semantic::Registry>>>,
    Rc<Cell<u64>>,
);

impl Gallery {
    /// An empty gallery.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enrol_native(&self, registry: super::super::semantic::Registry) {
        self.1.replace(Some(registry));
    }

    pub fn native_semantics(&self, slot: &str) -> Result<hl_extension::PaneSemanticTree, hl_extension::HostError> {
        if slot != "workspace" {
            return Err(hl_extension::HostError::Absent(slot.to_owned()));
        }
        let registry = self
            .1
            .borrow()
            .clone()
            .ok_or_else(|| hl_extension::HostError::Absent("workspace has no native semantic pane".into()))?;
        let snapshot = registry.snapshot();
        Ok(hl_extension::PaneSemanticTree {
            slot: snapshot.slot,
            generation: 0,
            revision: snapshot.revision,
            root: native_node(snapshot.root),
            truncated: snapshot.truncated,
        })
    }

    pub fn native_action(&self, action: &hl_extension::PaneSemanticAction) -> Result<(), hl_extension::HostError> {
        if action.generation != 0 {
            return Err(hl_extension::HostError::Conflict(format!(
                "stale pane generation {}; current is 0",
                action.generation
            )));
        }
        let registry = self
            .1
            .borrow()
            .clone()
            .ok_or_else(|| hl_extension::HostError::Absent("workspace has no native semantic pane".into()))?;
        registry
            .act(&super::super::semantic::Action {
                revision: action.revision,
                node: action.node,
                action: native_action(action.action),
                value: action.value.clone(),
            })
            .map_err(|refusal| match refusal {
                super::super::semantic::Refusal::Absent(node) => hl_extension::HostError::Absent(node.to_string()),
                other => hl_extension::HostError::Conflict(format!("native semantic action refused: {other:?}")),
            })
    }

    pub fn native_requirement(&self, node: u64) -> Result<hl_extension::Capability, hl_extension::HostError> {
        self.1
            .borrow()
            .as_ref()
            .ok_or_else(|| hl_extension::HostError::Absent("workspace has no native semantic pane".into()))?
            .requirement(node)
            .map_err(|refusal| hl_extension::HostError::Absent(format!("native semantic node refused: {refusal:?}")))
    }

    /// Records where one extension's interface is, replacing whatever was
    /// recorded under that name — a rebuilt page is a new interface.
    pub fn enrol(
        &self,
        extension: &str,
        interface: &impl IsA<gtk::Widget>,
        home: &gtk::Box,
        providers: &[hl_extension::PaneProvider],
        selected: Rc<dyn Fn(hl_extension::PaneSelection)>,
    ) -> u64 {
        let generation = self.2.get().wrapping_add(1).max(1);
        self.2.set(generation);
        let exhibit = Exhibit {
            generation,
            interface: interface.as_ref().downgrade(),
            home: home.downgrade(),
            providers: providers.to_vec(),
            ready: false,
            selected,
            semantics: None,
            action: None,
            pane: None,
            retire: None,
            shutdown: None,
        };
        self.0.borrow_mut().insert(extension.to_owned(), exhibit);
        generation
    }

    /// Publishes this generation's declared pane providers after its first
    /// successfully reconciled interface frame.
    pub fn ready(&self, extension: &str, generation: u64) {
        if let Some(exhibit) = self
            .0
            .borrow_mut()
            .get_mut(extension)
            .filter(|exhibit| exhibit.generation == generation)
        {
            exhibit.ready = true;
        }
    }

    pub fn enrol_semantics(
        &self,
        extension: &str,
        semantics: Rc<dyn Fn(&str) -> Result<hl_extension::PaneSemanticTree, hl_extension::HostError>>,
        action: Rc<dyn Fn(&str, &hl_extension::PaneSemanticAction) -> Result<(), hl_extension::HostError>>,
    ) {
        if let Some(exhibit) = self.0.borrow_mut().get_mut(extension) {
            exhibit.semantics = Some(semantics);
            exhibit.action = Some(action);
        }
    }

    /// Connects stable pane slots to independently retained renderer trees.
    pub fn enrol_panes(&self, extension: &str, pane: Rc<dyn Fn(&str) -> gtk::Widget>) {
        if let Some(exhibit) = self.0.borrow_mut().get_mut(extension) {
            exhibit.pane = Some(pane);
        }
    }

    pub fn enrol_retirement(&self, extension: &str, retire: Rc<dyn Fn(&str)>) {
        if let Some(exhibit) = self.0.borrow_mut().get_mut(extension) {
            exhibit.retire = Some(retire);
        }
    }

    /// Binds immediate authority invalidation to lifecycle withdrawal. The
    /// callback must not wait; owned teardown continues on the host driver.
    pub fn enrol_shutdown(&self, extension: &str, shutdown: Rc<dyn Fn()>) {
        if let Some(exhibit) = self.0.borrow_mut().get_mut(extension) {
            exhibit.shutdown = Some(shutdown);
        }
    }

    pub fn retire(&self, extension: &str, slot: &str) {
        let endpoint = self.0.borrow().get(extension).and_then(|entry| entry.retire.clone());
        if let Some(endpoint) = endpoint {
            endpoint(slot);
        }
    }

    pub fn semantics(
        &self,
        extension: &str,
        slot: &str,
    ) -> Result<hl_extension::PaneSemanticTree, hl_extension::HostError> {
        let held = self.0.borrow();
        let generation = held.get(extension).map(|entry| entry.generation);
        let endpoint = held.get(extension).and_then(|entry| entry.semantics.clone());
        drop(held);
        let generation = generation.ok_or_else(|| hl_extension::HostError::Absent(extension.to_owned()))?;
        let Some(endpoint) = endpoint else {
            let mut tree = unavailable(slot, &format!("{extension} has no semantic projection"));
            tree.generation = generation;
            return Ok(tree);
        };
        match endpoint(slot) {
            Ok(mut tree) => {
                tree.generation = generation;
                Ok(tree)
            }
            Err(hl_extension::HostError::Absent(detail) | hl_extension::HostError::Unsupported(detail)) => {
                let mut tree = unavailable(slot, &detail);
                tree.generation = generation;
                Ok(tree)
            }
            Err(error) => Err(error),
        }
    }

    pub fn semantic_action(
        &self,
        extension: &str,
        slot: &str,
        action: &hl_extension::PaneSemanticAction,
    ) -> Result<(), hl_extension::HostError> {
        let held = self.0.borrow();
        let exhibit = held
            .get(extension)
            .ok_or_else(|| hl_extension::HostError::Absent(format!("{extension} has no semantic surface")))?;
        if action.generation != exhibit.generation {
            return Err(hl_extension::HostError::Conflict(format!(
                "stale pane generation {}; current is {}",
                action.generation, exhibit.generation
            )));
        }
        let endpoint = exhibit
            .action
            .clone()
            .ok_or_else(|| hl_extension::HostError::Absent(format!("{extension} has no semantic surface")))?;
        drop(held);
        endpoint(slot, action)
    }

    pub fn generation(&self, extension: &str) -> Option<u64> {
        self.0.borrow().get(extension).map(|entry| entry.generation)
    }

    /// Stops advertising an extension whose lifecycle page is being removed.
    /// Pane restoration runs first, so any lent interface has already returned
    /// home before this final strong callback and provider list are forgotten.
    pub fn withdraw(&self, extension: &str) {
        let exhibit = self.0.borrow_mut().remove(extension);
        if let Some(exhibit) = exhibit {
            // Keep the callbacks that own the host alive until its weak
            // shutdown endpoint has invalidated the conversation.
            if let Some(shutdown) = exhibit.shutdown.as_ref() {
                shutdown();
            }
        }
    }

    /// Reports a provider choice to the extension that owns it.
    pub fn select(&self, extension: &str, provider: &str, slot: &str) {
        let Some(generation) = self.generation(extension) else {
            return;
        };
        self.select_at(extension, generation, provider, slot);
    }

    /// Reports a choice only while the exact generation that advertised it is
    /// still authoritative. A popover may outlive an extension replacement.
    pub fn select_at(&self, extension: &str, generation: u64, provider: &str, slot: &str) {
        let held = self.0.borrow();
        let Some(exhibit) = held.get(extension).filter(|entry| entry.generation == generation) else {
            return;
        };
        let Some(provider) = exhibit
            .providers
            .iter()
            .find(|candidate| candidate.id.as_str() == provider)
        else {
            return;
        };
        (exhibit.selected)(hl_extension::PaneSelection {
            pane_provider: provider.id.clone(),
            slot: slot.to_owned(),
        });
    }

    /// Whether this live extension declared this provider.
    #[must_use]
    pub fn offers(&self, extension: &str, provider: &str) -> bool {
        self.generation(extension)
            .is_some_and(|generation| self.offers_at(extension, generation, provider))
    }

    /// Whether the exact live generation that created a chooser action still
    /// offers its provider.
    #[must_use]
    pub fn offers_at(&self, extension: &str, generation: u64, provider: &str) -> bool {
        self.0.borrow().get(extension).is_some_and(|exhibit| {
            exhibit.generation == generation
                && exhibit.interface.upgrade().is_some()
                && exhibit.ready
                && exhibit.semantics.is_some()
                && exhibit
                    .providers
                    .iter()
                    .any(|candidate| candidate.id.as_str() == provider)
        })
    }

    /// Every live provider in deterministic extension/manifest order.
    #[must_use]
    pub fn providers(&self) -> Vec<Provider> {
        let held = self.0.borrow();
        let mut extensions: Vec<_> = held.iter().collect();
        extensions.sort_by_key(|(name, _)| *name);
        extensions
            .into_iter()
            // A provider is not inspectable merely because it has pixels. Do
            // not advertise it until its retained semantic projection is
            // registered alongside the widget it will place in the pane.
            .filter(|(_, exhibit)| {
                exhibit.interface.upgrade().is_some() && exhibit.ready && exhibit.semantics.is_some()
            })
            .flat_map(|(extension, exhibit)| {
                exhibit.providers.iter().map(move |provider| Provider {
                    extension: extension.clone(),
                    generation: exhibit.generation,
                    id: provider.id.to_string(),
                    title: provider.title.clone(),
                    icon: provider.icon.clone(),
                })
            })
            .collect()
    }

    /// Takes one extension's interface out of its page, for a pane to hold.
    ///
    /// `None` when nothing is recorded under that name or the page has gone,
    /// which is what an extension that is not installed here looks like.
    #[must_use]
    pub fn lend(&self, extension: &str) -> Option<gtk::Widget> {
        let held = self.0.borrow();
        let exhibit = held.get(extension)?;
        let interface = exhibit.interface.upgrade()?;
        if let Some(parent) = interface.parent().and_downcast::<gtk::Box>() {
            parent.remove(&interface);
        }
        Some(interface)
    }

    /// The independently retained interface for an addressed pane slot.
    #[must_use]
    pub fn pane(&self, extension: &str, slot: &str) -> Option<gtk::Widget> {
        let held = self.0.borrow();
        let pane = held.get(extension)?.pane.clone();
        drop(held);
        let Some(pane) = pane else {
            return self.lend(extension);
        };
        let interface = pane(slot);
        if let Some(parent) = interface.parent().and_downcast::<gtk::Box>() {
            parent.remove(&interface);
        }
        Some(interface)
    }

    /// Puts one extension's interface back on its page.
    pub fn recover(&self, extension: &str, interface: &gtk::Widget) {
        let held = self.0.borrow();
        let Some(home) = held.get(extension).and_then(|exhibit| exhibit.home.upgrade()) else {
            return;
        };
        if interface.parent().is_none() {
            home.append(interface);
        }
    }

    /// Releases a closed slot without moving another retained pane surface.
    pub fn release(&self, extension: &str, interface: &gtk::Widget) {
        let independently_retained = self
            .0
            .borrow()
            .get(extension)
            .is_some_and(|exhibit| exhibit.pane.is_some());
        if !independently_retained {
            self.recover(extension, interface);
        }
    }

    /// Whether an interface is recorded under this name and still drawable.
    #[must_use]
    pub fn holds(&self, extension: &str) -> bool {
        self.0
            .borrow()
            .get(extension)
            .is_some_and(|exhibit| exhibit.interface.upgrade().is_some())
    }

    #[must_use]
    pub fn retains_panes(&self, extension: &str) -> bool {
        self.0
            .borrow()
            .get(extension)
            .is_some_and(|exhibit| exhibit.pane.is_some())
    }
}

/// A bounded, typed answer for a retained surface that genuinely cannot
/// provide its authored tree (for example a restored pane whose sidecar is not
/// running). It remains inspectable without pretending terminal text exists.
fn unavailable(slot: &str, detail: &str) -> hl_extension::PaneSemanticTree {
    hl_extension::PaneSemanticTree {
        slot: slot.to_owned(),
        generation: 0,
        revision: 0,
        root: hl_extension::SemanticNode {
            id: 0,
            role: "status".to_owned(),
            label: Some("Interface unavailable".to_owned()),
            value: Some(detail.chars().take(hl_extension::port::SEMANTIC_TEXT_LIMIT).collect()),
            disabled: true,
            destructive: false,
            actions: Vec::new(),
            children: Vec::new(),
        },
        truncated: detail.chars().count() > hl_extension::port::SEMANTIC_TEXT_LIMIT,
    }
}

fn native_node(node: super::super::semantic::Node) -> hl_extension::SemanticNode {
    hl_extension::SemanticNode {
        id: node.id,
        role: node.role,
        label: node.label,
        value: node.value,
        disabled: node.disabled,
        destructive: node.destructive,
        actions: node.actions.into_iter().map(wire_action).collect(),
        children: node.children.into_iter().map(native_node).collect(),
    }
}

fn wire_action(action: super::super::semantic::ActionKind) -> hl_extension::SemanticActionKind {
    use super::super::semantic::ActionKind;
    match action {
        ActionKind::Invoke => hl_extension::SemanticActionKind::Invoke,
        ActionKind::Change => hl_extension::SemanticActionKind::Change,
        ActionKind::Submit => hl_extension::SemanticActionKind::Submit,
        ActionKind::Toggle => hl_extension::SemanticActionKind::Toggle,
        ActionKind::Expand => hl_extension::SemanticActionKind::Expand,
        ActionKind::Focus => hl_extension::SemanticActionKind::Focus,
    }
}

fn native_action(action: hl_extension::SemanticActionKind) -> super::super::semantic::ActionKind {
    use super::super::semantic::ActionKind;
    match action {
        hl_extension::SemanticActionKind::Invoke => ActionKind::Invoke,
        hl_extension::SemanticActionKind::Change => ActionKind::Change,
        hl_extension::SemanticActionKind::Submit => ActionKind::Submit,
        hl_extension::SemanticActionKind::Toggle => ActionKind::Toggle,
        hl_extension::SemanticActionKind::Expand => ActionKind::Expand,
        hl_extension::SemanticActionKind::Focus => ActionKind::Focus,
    }
}

impl std::fmt::Debug for Gallery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Gallery")
            .field("exhibits", &self.0.borrow().len())
            .finish_non_exhaustive()
    }
}
