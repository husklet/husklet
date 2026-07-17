//! Toolkit-neutral GUI components and event contracts.
//!
//! Applications supply view trees and interpret emitted events. This crate deliberately contains no
//! container, engine, GPU, workspace-policy, persistence, or product orchestration.

mod component;
mod settings;

pub use component::{Component, Element, Event, EventId, Events, ListItem, View};
pub use settings::{Choice, Field, FieldId, FieldKind, Settings, Value};

/// A toolkit adapter renders a declarative view and reports interaction through an event sink.
pub trait Renderer {
    type Error;

    fn render(&mut self, view: &View, events: &mut dyn Events) -> Result<(), Self::Error>;
}
