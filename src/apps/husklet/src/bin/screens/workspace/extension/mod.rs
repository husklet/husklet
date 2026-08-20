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

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use hl_gui::{Event, Frame, Renderer, SourceMutation, Tree};
use hl_gui_gtk::Surface;

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
    banner: Banner,
    deliveries: Deliveries,
    sink: Rc<dyn Sink>,
    /// Monotonic tick count, which is the clock the row models age against.
    clock: u64,
}

impl Interface {
    /// Builds the page, handing back the widget to place and the interface that
    /// drives it.
    #[must_use]
    pub fn new(deliveries: Deliveries, sink: Rc<dyn Sink>) -> (gtk::Box, Self) {
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
            banner,
            deliveries,
            sink,
            clock: 0,
        };
        (widget, interface)
    }

    /// Puts the page on the main loop. The tick ends with the widget.
    pub fn install(self) {
        let interface = Rc::new(RefCell::new(self));
        glib::timeout_add_local(TICK, move || {
            let mut page = interface.borrow_mut();
            if page.page.upgrade().is_none() {
                return glib::ControlFlow::Break;
            }
            page.tick();
            glib::ControlFlow::Continue
        });
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
            Delivery::Loss(reason) => self.banner.show(&reason),
        }
    }

    /// Applies one frame. A rejected frame means the producer and the tree no
    /// longer agree, which the user sees as the extension having stopped.
    fn draw(&mut self, frame: &Frame) {
        // A frame arriving means the extension is speaking again, so a banner
        // from an earlier loss no longer describes what is on screen.
        self.banner.hide();
        if let Err(fault) = self.tree.apply(frame, &mut self.surface) {
            self.banner.show(&fault.to_string());
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

    /// Hands interaction to the sink.
    fn report(&self) {
        for event in self.surface.reports().drain() {
            self.sink.accept(Signal::Interaction(event));
        }
    }

    /// Hands the row windows the tables decided they need to the same sink, so
    /// the host answers them over the same path it answers interaction.
    fn request(&mut self) {
        self.clock = self.clock.wrapping_add(1);
        for request in self.surface.requests(self.clock) {
            self.sink.accept(Signal::Interaction(Event::Rows(request)));
        }
    }
}
