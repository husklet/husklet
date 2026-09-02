//! The workspace page that hosts an extension's interface.
//!
//! An extension describes its interface over a socket on another thread. This
//! page owns the toolkit side of that: it drains what the other thread posted,
//! drives an [`hl_gui::Tree`] over an [`hl_gui_gtk::Surface`], and hands
//! interaction and row requests back through a [`Sink`] the caller supplies.
//!
//! It never names the extension host. That keeps the screen testable against a
//! plain closure, and keeps the two halves independent.

mod banner;
mod queue;
mod sink;

#[cfg(test)]
mod test;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use hl_extension::{HostError, PaneSemanticAction, PaneSemanticTree, SemanticActionKind, SemanticNode};
use hl_gui::{Event, Frame, Renderer, SourceMutation, Tree};
use hl_gui_gtk::Surface;

const FAULT_NODE: u64 = u64::MAX;

pub use banner::Banner;
pub use queue::{channel, Deliveries, Delivery, Post, CAPACITY, DRAIN};
pub use sink::{Signal, Sink};

/// How often the page looks at its queue. Matches the other live workspace
/// pages, so one extension does not set the application's rhythm.
const TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// The extension page: a frozen-capable banner above a rendered surface.
pub struct Interface {
    /// Held weakly so a page removed from the shell stops ticking. The shell
    /// and the caller hold the widget itself.
    page: glib::WeakRef<gtk::Box>,
    surface: Surface,
    tree: Tree,
    panes: HashMap<String, PaneInterface>,
    banner: Banner,
    /// A retry is one request, not a repeatable action while the host reopens.
    recovery_pending: Cell<bool>,
    deliveries: Deliveries,
    sink: Rc<dyn Sink>,
    faulted: Rc<dyn Fn(u32)>,
    ready: Rc<dyn Fn()>,
    /// Monotonic tick count, which is the clock the row models age against.
    clock: u64,
}

struct PaneInterface {
    surface: Surface,
    tree: Tree,
}

impl PaneInterface {
    fn new() -> Self {
        Self {
            surface: Surface::new(),
            tree: Tree::new(),
        }
    }
}

impl Interface {
    /// Builds the page, handing back the widget to place and the interface that
    /// drives it.
    #[must_use]
    pub fn new(deliveries: Deliveries, sink: Rc<dyn Sink>) -> (gtk::Box, Self) {
        Self::with_lifecycle(deliveries, sink, Rc::new(|_| {}), Rc::new(|| {}))
    }

    /// Builds a page that also publishes structured crash-loop state on the
    /// toolkit thread. The callback may therefore update GTK and the roster;
    /// the background host never touches either one.
    #[must_use]
    pub fn with_faults(deliveries: Deliveries, sink: Rc<dyn Sink>, faulted: Rc<dyn Fn(u32)>) -> (gtk::Box, Self) {
        Self::with_lifecycle(deliveries, sink, faulted, Rc::new(|| {}))
    }

    /// Builds a page whose provider authority is published only after a valid
    /// frame proves this host generation can render.
    #[must_use]
    pub fn with_lifecycle(
        deliveries: Deliveries,
        sink: Rc<dyn Sink>,
        faulted: Rc<dyn Fn(u32)>,
        ready: Rc<dyn Fn()>,
    ) -> (gtk::Box, Self) {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        let banner = Banner::new(sink.clone());
        widget.append(banner.widget());
        let surface = Surface::new();
        widget.append(surface.widget());
        let interface = Self {
            page: widget.downgrade(),
            surface,
            tree: Tree::new(),
            panes: HashMap::new(),
            banner,
            recovery_pending: Cell::new(false),
            deliveries,
            sink,
            faulted,
            ready,
            clock: 0,
        };
        (widget, interface)
    }

    /// Puts the page on the main loop. The tick ends with the widget.
    pub fn install(self) -> Rc<RefCell<Self>> {
        let interface = Rc::new(RefCell::new(self));
        let installed = Rc::clone(&interface);
        glib::timeout_add_local(TICK, move || {
            let mut page = interface.borrow_mut();
            if page.page.upgrade().is_none() {
                return glib::ControlFlow::Break;
            }
            page.tick();
            glib::ControlFlow::Continue
        });
        installed
    }

    pub fn semantics(&self, slot: &str) -> Result<PaneSemanticTree, HostError> {
        let tree = self.panes.get(slot).map_or(&self.tree, |pane| &pane.tree);
        self.semantics_from(tree, slot)
    }

    fn semantics_from(&self, tree: &Tree, slot: &str) -> Result<PaneSemanticTree, HostError> {
        let mut count = 0;
        let mut truncated = false;
        let mut root = Self::semantic_node(tree, hl_gui::NodeId::ROOT, 0, &mut count, &mut truncated)?;
        if self.banner.is_visible() {
            if count >= hl_extension::port::SEMANTIC_NODE_LIMIT {
                root.children.pop();
                truncated = true;
            }
            root.children.insert(
                0,
                SemanticNode {
                    id: FAULT_NODE,
                    role: "alert".into(),
                    label: Some("Extension stopped".into()),
                    value: Some(
                        self.banner
                            .text()
                            .chars()
                            .take(hl_extension::port::SEMANTIC_TEXT_LIMIT)
                            .collect(),
                    ),
                    disabled: self.recovery_pending.get(),
                    destructive: false,
                    actions: vec![SemanticActionKind::Invoke],
                    children: Vec::new(),
                },
            );
        }
        Ok(PaneSemanticTree {
            slot: slot.to_owned(),
            revision: self.semantic_revision(tree),
            root,
            truncated,
        })
    }

    fn semantic_node(
        tree: &Tree,
        id: hl_gui::NodeId,
        depth: usize,
        count: &mut usize,
        truncated: &mut bool,
    ) -> Result<SemanticNode, HostError> {
        if depth >= hl_extension::port::SEMANTIC_DEPTH_LIMIT || *count >= hl_extension::port::SEMANTIC_NODE_LIMIT {
            *truncated = true;
            return Err(HostError::Conflict("semantic tree exceeds its bounded shape".into()));
        }
        *count += 1;
        let node = tree
            .node(id)
            .ok_or_else(|| HostError::Absent(format!("semantic node {}", id.raw())))?;
        let secret = node.flag(hl_gui::Prop::Secret, false);
        let clip = |value: &str| {
            value
                .chars()
                .take(hl_extension::port::SEMANTIC_TEXT_LIMIT)
                .collect::<String>()
        };
        let actions = node
            .is_enabled()
            .then_some(node.handlers.keys())
            .into_iter()
            .flatten()
            .filter_map(|trigger| match trigger {
                hl_gui::Trigger::Invoke => Some(SemanticActionKind::Invoke),
                hl_gui::Trigger::Change => Some(SemanticActionKind::Change),
                hl_gui::Trigger::Submit => Some(SemanticActionKind::Submit),
                hl_gui::Trigger::Focus => Some(SemanticActionKind::Focus),
                hl_gui::Trigger::Toggle => Some(SemanticActionKind::Toggle),
                hl_gui::Trigger::Expand => Some(SemanticActionKind::Expand),
                _ => None,
            })
            .collect();
        let mut children = Vec::new();
        for child in &node.children {
            if tree.node(*child).is_some_and(|node| !node.is_visible()) {
                continue;
            }
            if depth + 1 >= hl_extension::port::SEMANTIC_DEPTH_LIMIT
                || *count >= hl_extension::port::SEMANTIC_NODE_LIMIT
            {
                *truncated = true;
                break;
            }
            children.push(Self::semantic_node(tree, *child, depth + 1, count, truncated)?);
        }
        Ok(SemanticNode {
            id: id.raw(),
            role: node.tag.as_str().to_owned(),
            label: node.text(hl_gui::Prop::Label).map(clip),
            value: node
                .text(hl_gui::Prop::Value)
                .map(|value| if secret { "[redacted]".to_owned() } else { clip(value) }),
            disabled: !node.is_enabled(),
            destructive: node.flag(hl_gui::Prop::Destructive, false),
            actions,
            children,
        })
    }

    pub fn semantic_action(&self, action: &PaneSemanticAction) -> Result<(), HostError> {
        self.semantic_action_at("", action)
    }

    pub fn semantic_action_at(&self, slot: &str, action: &PaneSemanticAction) -> Result<(), HostError> {
        let tree = self.panes.get(slot).map_or(&self.tree, |pane| &pane.tree);
        let revision = self.semantic_revision(tree);
        if action.revision != revision {
            return Err(HostError::Conflict(format!(
                "stale semantic revision {}; current is {}",
                action.revision, revision
            )));
        }
        if action.node == FAULT_NODE && action.action == SemanticActionKind::Invoke && self.banner.is_visible() {
            if self.recovery_pending.replace(true) {
                return Err(HostError::Conflict("extension recovery is already pending".into()));
            }
            self.banner.pending();
            self.sink.accept(Signal::Retry);
            return Ok(());
        }
        let node_id = hl_gui::NodeId::new(action.node);
        let trigger = match action.action {
            SemanticActionKind::Invoke => hl_gui::Trigger::Invoke,
            SemanticActionKind::Change => hl_gui::Trigger::Change,
            SemanticActionKind::Submit => hl_gui::Trigger::Submit,
            SemanticActionKind::Focus => hl_gui::Trigger::Focus,
            SemanticActionKind::Toggle => hl_gui::Trigger::Toggle,
            SemanticActionKind::Expand => hl_gui::Trigger::Expand,
        };
        let id = tree
            .node(node_id)
            .and_then(|node| node.action(trigger))
            .cloned()
            .ok_or_else(|| HostError::Conflict("node is hidden, disabled, or does not declare that action".into()))?;
        let event = match action.action {
            SemanticActionKind::Invoke => Event::Invoke { node: node_id, id },
            SemanticActionKind::Submit => Event::Submit { node: node_id, id },
            SemanticActionKind::Focus => Event::Focus {
                node: node_id,
                id,
                focused: action.value.as_deref() != Some("false"),
            },
            SemanticActionKind::Change => Event::Change {
                node: node_id,
                id,
                value: hl_gui::PropValue::text(action.value.clone().unwrap_or_default()),
            },
            SemanticActionKind::Toggle | SemanticActionKind::Expand => Event::Change {
                node: node_id,
                id,
                value: hl_gui::PropValue::Flag(action.value.as_deref() != Some("false")),
            },
        };
        if self.panes.contains_key(slot) {
            self.sink.accept(Signal::InteractionAt {
                slot: slot.to_owned(),
                event,
            });
        } else {
            self.sink.accept(Signal::Interaction(event));
        }
        Ok(())
    }

    fn semantic_revision(&self, tree: &Tree) -> u64 {
        let state = if self.recovery_pending.get() {
            2
        } else if self.banner.is_visible() {
            1
        } else {
            0
        };
        tree.sequence().saturating_mul(3).saturating_add(state)
    }

    /// Returns the independently retained widget for one stable pane slot.
    pub fn pane(&mut self, slot: &str) -> gtk::Widget {
        let pane = self.panes.entry(slot.to_owned()).or_insert_with(PaneInterface::new);
        pane.surface.widget().clone().upcast()
    }

    /// Retires one independently addressed renderer and drops any interaction
    /// it had queued before its terminal pane disappeared.
    pub fn retire(&mut self, slot: &str) {
        self.panes.remove(slot);
    }

    /// One turn: apply what is queued, then hand back what the surface says.
    ///
    /// Returns how many deliveries were applied, which is at most [`DRAIN`].
    pub fn tick(&mut self) -> usize {
        let applied = self.drain();
        self.report();
        self.request();
        applied
    }

    /// The banner, for tests and diagnostics.
    #[must_use]
    pub const fn banner(&self) -> &Banner {
        &self.banner
    }

    /// The rendered surface, for tests and diagnostics.
    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    /// Applies at most [`DRAIN`] deliveries, leaving the rest queued.
    fn drain(&mut self) -> usize {
        let mut applied = 0;
        while applied < DRAIN {
            // A closed queue is not a loss on its own: the host reports why it
            // ended through `Delivery::Loss`, and stale work may still be here.
            let Ok(delivery) = self.deliveries.try_recv() else {
                break;
            };
            self.accept(delivery);
            applied += 1;
        }
        applied
    }

    fn accept(&mut self, delivery: Delivery) {
        match delivery {
            Delivery::Frame(frame) => self.draw(&frame),
            Delivery::Source(mutation) => self.feed(&mutation),
            Delivery::FrameAt { slot, frame } => self.draw_at(&slot, &frame),
            Delivery::SourceAt { slot, mutation } => self.feed_at(&slot, &mutation),
            Delivery::Loss(reason) => {
                self.recovery_pending.set(false);
                self.banner.show(&reason);
            }
            Delivery::Fault { restarts } => (self.faulted)(restarts),
        }
    }

    /// Applies one frame. A rejected frame means the producer and the tree no
    /// longer agree, which the user sees as the extension having stopped.
    fn draw(&mut self, frame: &Frame) {
        match self.tree.apply(frame, &mut self.surface) {
            Ok(()) => {
                (self.ready)();
                self.recovery_pending.set(false);
                self.banner.hide();
            }
            Err(fault) => self.banner.show(&fault.to_string()),
        }
    }

    fn draw_at(&mut self, slot: &str, frame: &Frame) {
        let pane = self.panes.entry(slot.to_owned()).or_insert_with(PaneInterface::new);
        match pane.tree.apply(frame, &mut pane.surface) {
            Ok(()) => {
                (self.ready)();
                self.recovery_pending.set(false);
                self.banner.hide();
            }
            Err(fault) => self.banner.show(&fault.to_string()),
        }
    }

    /// Applies one source mutation. A mutation for a source no table is bound
    /// to is ordinary — the table was removed while it was in flight.
    fn feed(&mut self, mutation: &SourceMutation) {
        match mutation {
            SourceMutation::Length { source, version, rows } => {
                drop(self.surface.resize(*source, *version, *rows));
            }
            SourceMutation::Window(window) => drop(self.surface.rows(window)),
            _ => {}
        }
    }

    fn feed_at(&mut self, slot: &str, mutation: &SourceMutation) {
        let Some(pane) = self.panes.get_mut(slot) else { return };
        match mutation {
            SourceMutation::Length { source, version, rows } => {
                drop(pane.surface.resize(*source, *version, *rows));
            }
            SourceMutation::Window(window) => drop(pane.surface.rows(window)),
            _ => {}
        }
    }

    /// Hands interaction to the sink.
    fn report(&self) {
        for event in self.surface.reports().drain() {
            self.sink.accept(Signal::Interaction(event));
        }
        for (slot, pane) in &self.panes {
            for event in pane.surface.reports().drain() {
                self.sink.accept(Signal::InteractionAt {
                    slot: slot.clone(),
                    event,
                });
            }
        }
    }

    /// Hands the row windows the tables decided they need to the same sink, so
    /// the host answers them over the same path it answers interaction.
    fn request(&mut self) {
        self.clock = self.clock.wrapping_add(1);
        for request in self.surface.requests(self.clock) {
            self.sink.accept(Signal::Interaction(Event::Rows(request)));
        }
        for (slot, pane) in &mut self.panes {
            for request in pane.surface.requests(self.clock) {
                self.sink.accept(Signal::InteractionAt {
                    slot: slot.clone(),
                    event: Event::Rows(request),
                });
            }
        }
    }
}
