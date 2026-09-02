use super::*;

pub(crate) struct Slots<'a>(&'a Rc<TermWin>);

impl<'a> Slots<'a> {
    pub(crate) fn new(window: &'a Rc<TermWin>) -> Self {
        Self(window)
    }

    pub(crate) fn allocate(&self) -> String {
        let tw = self.0;
        let n = tw.slot_ctr.get();
        tw.slot_ctr.set(n + 1);
        n.to_string()
    }

    /// Reuse a pane's saved slot and keep the allocator ahead of numeric restored slots.
    pub(crate) fn adopt(&self, saved: Option<&str>) -> String {
        let tw = self.0;
        let Some(saved) = saved else {
            return self.allocate();
        };
        if let Ok(slot) = saved.parse::<u32>() {
            if slot >= tw.slot_ctr.get() {
                tw.slot_ctr.set(slot + 1);
            }
        }
        saved.to_owned()
    }

    /// Register a pane holding a shell under its layout slot.
    pub(crate) fn hold(&self, terminal: &vte4::Terminal, slot: String) {
        self.0.panes.borrow_mut().push(PaneRegistration::new(terminal, slot));
    }

    /// Find the layout slot registered for `term` (pruning dead registry entries as it scans).
    pub(crate) fn of(&self, term: &vte4::Terminal) -> Option<String> {
        let tw = self.0;
        let mut found = None;
        tw.panes.borrow_mut().retain(|pane| match pane.terminal.upgrade() {
            Some(t) if &t == term => {
                found = Some(pane.slot.clone());
                true
            }
            Some(_) => true,
            None => false, // prune a dead pane
        });
        found
    }

    /// A pane closed by the user is dropped from the live registry.
    pub(crate) fn discard(&self, term: &vte4::Terminal) {
        let tw = self.0;
        tw.panes.borrow_mut().retain(|pane| match pane.terminal.upgrade() {
            Some(t) if &t == term => false,
            Some(_) => true,
            None => false, // prune dead entries while we're here
        });
    }

    /// Discard the slots of every terminal under a page's widget subtree (a whole tab being closed).
    pub(crate) fn discard_page(&self, child: &gtk::Widget) {
        let mut terms = Vec::new();
        PaneView::collect(child, &mut terms);
        for t in &terms {
            self.discard(t);
        }
        for pane in Panes::under(self.0, child) {
            // A closing tab hands any interface it held back to its own page.
            Surface::retire(self.0, &pane.content);
            self.release(&pane.content);
        }
    }

    /// Register a pane holding one extension's interface.
    pub(crate) fn enrol(&self, widget: &gtk::Widget, slot: String, extension: String, provider: Option<String>) {
        self.0
            .surfaces
            .borrow_mut()
            .push(SurfaceRegistration::new(widget, slot, extension, provider));
    }

    /// The slot and the extension registered for a surface pane, if it is one
    /// (pruning dead registry entries as it scans).
    pub(crate) fn surface(&self, widget: &gtk::Widget) -> Option<(String, String, Option<String>)> {
        let mut found = None;
        self.0.surfaces.borrow_mut().retain(|pane| match pane.widget.upgrade() {
            Some(held) if &held == widget => {
                found = Some((pane.slot.clone(), pane.extension.clone(), pane.provider.clone()));
                true
            }
            Some(_) => true,
            None => false, // prune a dead pane
        });
        found
    }

    /// A surface pane that closed is dropped from the live registry.
    pub(crate) fn release(&self, widget: &gtk::Widget) {
        self.0.surfaces.borrow_mut().retain(|pane| match pane.widget.upgrade() {
            Some(held) if &held == widget => false,
            Some(_) => true,
            None => false, // prune dead entries while we're here
        });
    }
}
