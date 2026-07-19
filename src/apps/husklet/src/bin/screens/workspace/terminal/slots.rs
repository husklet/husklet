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

    /// Reuse a pane's saved slot on restore (or allocate a fresh one for a slot-less legacy session). Keeps
    /// the allocator ahead of any reused numeric slot so later new panes never collide with a restored one.
    pub(crate) fn adopt(&self, saved: &Option<String>) -> String {
        let tw = self.0;
        let Some(saved) = saved else {
            return self.allocate();
        };
        if let Ok(slot) = saved.parse::<u32>() {
            if slot >= tw.slot_ctr.get() {
                tw.slot_ctr.set(slot + 1);
            }
        }
        saved.clone()
    }

    /// Find the layout slot registered for `term` (pruning dead registry entries as it scans).
    pub(crate) fn of(&self, term: &vte4::Terminal) -> Option<String> {
        let tw = self.0;
        let mut found = None;
        tw.panes
            .borrow_mut()
            .retain(|(w, slot, _)| match w.upgrade() {
                Some(t) if &t == term => {
                    found = Some(slot.clone());
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
        tw.panes
            .borrow_mut()
            .retain(|(w, slot, _)| match w.upgrade() {
                Some(t) if &t == term => {
                    let _ = slot;
                    false
                }
                Some(_) => true,
                None => false, // prune dead entries while we're here
            });
    }

    /// Discard the slots of every terminal under a page's widget subtree (a whole tab being closed).
    pub(crate) fn discard_page(&self, child: &gtk::Widget) {
        let mut terms = Vec::new();
        TerminalPane::collect(child, &mut terms);
        for t in &terms {
            self.discard(t);
        }
    }
}
