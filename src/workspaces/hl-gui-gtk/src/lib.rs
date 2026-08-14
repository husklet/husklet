//! GTK4 rendering adapter for the `hl-gui` component library.
//!
//! This crate is the only place a toolkit type appears. It maps validated
//! mutations onto retained GTK widgets and reports interaction back as typed
//! events, so the component library — and anything describing an interface with
//! it — stays portable.

mod build;
mod collection;
mod event;
mod prop;
mod registry;
pub mod rows;
pub mod style;

pub use event::Reports;
pub use registry::Failure;
pub use rows::Rows;

use std::collections::HashMap;

use gtk::prelude::*;
use hl_gui::{Node, NodeId, Patch, Prop, PropValue, Renderer, RowWindow, SourceId, Theme, Tree};

use registry::Registry;

/// A rendered surface: the retained widget tree for one node tree.
pub struct Surface {
    registry: Registry,
    bindings: event::Bindings,
    sources: HashMap<SourceId, NodeId>,
    root: gtk::Box,
    reports: Reports,
}

impl Surface {
    /// Creates a surface whose root container is ready to be added to a window.
    #[must_use]
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("hl-root");
        root.set_hexpand(true);
        root.set_vexpand(true);
        let mut registry = Registry::default();
        registry.insert(NodeId::ROOT, root.clone().upcast());
        Self {
            registry,
            bindings: event::Bindings::default(),
            sources: HashMap::new(),
            root,
            reports: Reports::new(),
        }
    }

    /// The widget to place in a window.
    #[must_use]
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// Interaction reported since the previous drain.
    #[must_use]
    pub fn reports(&self) -> &Reports {
        &self.reports
    }

    /// Records how long a source is, which is what its scrollbar describes.
    ///
    /// # Errors
    /// Returns `Failure::Unmapped` when no node is bound to the source.
    pub fn resize(&mut self, source: SourceId, version: hl_gui::Version, rows: u64) -> Result<(), Failure> {
        let id = self.sources.get(&source).copied().ok_or(Failure::Unbound(source))?;
        let widget = self.require(id)?;
        if let Some(model) = collection::model(widget, source) {
            model.resize(version, rows);
        }
        Ok(())
    }

    /// Row windows the bound tables have decided they need, taken for sending.
    #[must_use]
    pub fn requests(&self, now: u64) -> Vec<hl_gui::RowRequest> {
        let mut pending = Vec::new();
        for (source, id) in &self.sources {
            let Ok(widget) = self.require(*id) else {
                continue;
            };
            let Some(model) = collection::model(widget, *source) else {
                continue;
            };
            model.tick(now);
            pending.extend(model.drain());
        }
        pending
    }

    /// Number of live widgets, for tests and diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.registry.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.registry.len() <= 1
    }

    fn require(&self, id: NodeId) -> Result<&gtk::Widget, Failure> {
        self.registry.get(id).ok_or(Failure::Unmapped(id))
    }

    fn create(&mut self, id: NodeId, tree: &Tree) -> Result<(), Failure> {
        let node = tree.node(id).ok_or(Failure::Unmapped(id))?;
        self.registry.insert(id, build::widget(node));
        Ok(())
    }

    fn place(&self, parent: NodeId, child: NodeId, index: usize) -> Result<(), Failure> {
        let host = self.require(parent)?;
        let widget = self.require(child)?;
        if collection::append(host, widget) {
            return Ok(());
        }
        if build::attach(host, widget, index) {
            return Ok(());
        }
        Err(Failure::NotAContainer(parent))
    }

    fn discard(&mut self, id: NodeId) -> Result<(), Failure> {
        let widget = self.registry.remove(id).ok_or(Failure::Unmapped(id))?;
        self.bindings.forget(id);
        build::detach(&widget);
        Ok(())
    }

    fn describe<'a>(&'a self, id: NodeId, tree: &'a Tree) -> Result<(&'a gtk::Widget, &'a Node), Failure> {
        let widget = self.require(id)?;
        let node = tree.node(id).ok_or(Failure::Unmapped(id))?;
        Ok((widget, node))
    }

    /// Where the child now sits among its siblings. The tree is already updated
    /// when this runs, so the child's own position is the answer.
    fn index(tree: &Tree, parent: NodeId, child: NodeId) -> usize {
        tree.node(parent)
            .and_then(|node| node.children.iter().position(|entry| *entry == child))
            .unwrap_or_default()
    }
}

impl Default for Surface {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer for Surface {
    type Error = Failure;

    fn patch(&mut self, patch: &Patch, tree: &Tree) -> Result<(), Self::Error> {
        match patch {
            Patch::Create { id, .. } => self.create(*id, tree),
            Patch::Insert { parent, child, .. } => {
                let index = Self::index(tree, *parent, *child);
                self.place(*parent, *child, index)
            }
            Patch::Move { parent, child, .. } => {
                let index = Self::index(tree, *parent, *child);
                build::detach(self.require(*child)?);
                self.place(*parent, *child, index)
            }
            Patch::SetProp { id, prop, value } => {
                if let (Prop::Source, PropValue::Source(source)) = (*prop, value) {
                    self.sources.insert(*source, *id);
                }
                let (widget, node) = self.describe(*id, tree)?;
                prop::apply(widget, node, *prop, value);
                Ok(())
            }
            Patch::ClearProp { id, prop } => {
                let (widget, node) = self.describe(*id, tree)?;
                prop::clear(widget, node, *prop);
                Ok(())
            }
            Patch::SetHandler { id, handler } => {
                let widget = self.registry.get(*id).ok_or(Failure::Unmapped(*id))?;
                self.bindings.set(widget, *id, handler, &self.reports);
                Ok(())
            }
            Patch::ClearHandler { id, trigger } => {
                self.bindings.clear(*id, *trigger);
                Ok(())
            }
            Patch::Remove { id } => self.discard(*id),
        }
    }

    fn commit(&mut self, _sequence: u64) -> Result<(), Self::Error> {
        self.root.queue_resize();
        Ok(())
    }

    fn rows(&mut self, window: &RowWindow) -> Result<(), Self::Error> {
        // A window for a source no node is bound to is stale, not an error:
        // the table was removed while the request was in flight.
        let Some(id) = self.sources.get(&window.source).copied() else {
            return Ok(());
        };
        collection::present(self.require(id)?, window);
        Ok(())
    }

    fn theme(&mut self, theme: &Theme) -> Result<(), Self::Error> {
        style::install(theme);
        Ok(())
    }
}
