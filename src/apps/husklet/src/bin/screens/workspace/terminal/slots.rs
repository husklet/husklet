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

    /// True if this pane slot has a frozen checkpoint on disk (a written MANIFEST) to restore.
    pub(crate) fn has_checkpoint(ws: &Workspace, slot: &str) -> bool {
        ws.checkpoint_slot_dir(&Home::current().root(), slot)
            .join("MANIFEST")
            .exists()
    }

    /// Find the checkpoint slot registered for `term` (pruning dead registry entries as it scans).
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

    /// A pane closed by the user (not a window close) → drop it from the registry and DISCARD its slot's
    /// stale checkpoint, so a later reopen doesn't wrongly resurrect a shell the user deliberately closed.
    pub(crate) fn discard(&self, term: &vte4::Terminal) {
        let tw = self.0;
        let mut removed = None;
        tw.panes
            .borrow_mut()
            .retain(|(w, slot, _)| match w.upgrade() {
                Some(t) if &t == term => {
                    removed = Some(slot.clone());
                    false
                }
                Some(_) => true,
                None => false, // prune dead entries while we're here
            });
        if let Some(slot) = removed {
            let _ =
                std::fs::remove_dir_all(tw.ws.checkpoint_slot_dir(&Home::current().root(), &slot));
        }
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
