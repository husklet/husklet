use super::*;

#[derive(Debug, Eq, PartialEq)]
struct FocusTransition {
    clear_previous: bool,
    update_current: bool,
}

impl FocusTransition {
    fn new(previous: bool, search_visible: bool) -> Self {
        Self {
            clear_previous: previous,
            update_current: search_visible,
        }
    }
}

impl Search {
    pub(crate) fn new() -> Self {
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bar.add_css_class("searchbar");
        bar.set_halign(gtk::Align::End);
        bar.set_valign(gtk::Align::Start);
        bar.set_visible(false);
        let entry = gtk::Entry::new();
        entry.add_css_class("searchfield");
        entry.set_placeholder_text(Some("Find"));
        entry.set_width_chars(22);
        let info = gtk::Label::new(None);
        info.add_css_class("searchinfo");
        bar.append(&entry);
        bar.append(&info);
        Self {
            bar,
            entry,
            info,
            caseless: Cell::new(true),
        }
    }

    pub(crate) fn wire(tw: &Rc<TermWin>) {
        {
            let tw = tw.clone();
            tw.search
                .entry
                .clone()
                .connect_changed(move |_| tw.search.update(tw.focused.borrow().clone()));
        }
        let kc = gtk::EventControllerKey::new();
        kc.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let tw = tw.clone();
            kc.connect_key_pressed(move |_, key, _c, state| {
                let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
                match key {
                    gdk::Key::Return | gdk::Key::KP_Enter => {
                        tw.search.step(tw.focused.borrow().clone(), !shift);
                        glib::Propagation::Stop
                    }
                    gdk::Key::Escape => {
                        tw.search.hide(tw.focused.borrow().clone());
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
        }
        tw.search.entry.add_controller(kc);
    }

    pub(crate) fn toggle(&self, terminal: Option<vte4::Terminal>) {
        if self.bar.get_visible() {
            self.hide(terminal);
        } else {
            self.bar.set_visible(true);
            self.entry.grab_focus();
            if !self.entry.text().is_empty() {
                self.update(terminal);
            }
        }
    }

    pub(crate) fn focus(&self, previous: Option<vte4::Terminal>, current: vte4::Terminal) {
        let transition = FocusTransition::new(previous.is_some(), self.bar.get_visible());
        if transition.clear_previous {
            if let Some(previous) = previous {
                previous.search_set_regex(None, 0);
                previous.unselect_all();
            }
        }
        if transition.update_current {
            self.update(Some(current));
        }
    }

    pub(crate) fn hide(&self, terminal: Option<vte4::Terminal>) {
        self.bar.set_visible(false);
        if let Some(t) = terminal {
            t.search_set_regex(None, 0);
            t.unselect_all();
            t.grab_focus();
        }
    }

    /// (Re)compile the query and jump to the first match. The query is tried as a regex; if it doesn't
    /// compile, it's escaped and matched literally (so plain text always "just works").
    pub(crate) fn update(&self, terminal: Option<vte4::Terminal>) {
        let Some(t) = terminal else {
            return;
        };
        let text = self.entry.text().to_string();
        if text.is_empty() {
            t.search_set_regex(None, 0);
            self.info.set_text("");
            self.info.remove_css_class("nomatch");
            return;
        }
        let mut flags = PCRE2_UTF | PCRE2_NO_UTF_CHECK | PCRE2_MULTILINE | PCRE2_UCP;
        if self.caseless.get() {
            flags |= PCRE2_CASELESS;
        }
        let re = vte4::Regex::for_search(&text, flags).or_else(|_| {
            let escaped = glib::Regex::escape_string(text.as_str());
            vte4::Regex::for_search(escaped.as_str(), flags)
        });
        match re {
            Ok(re) => {
                t.search_set_regex(Some(&re), 0);
                t.search_set_wrap_around(true);
                let found = t.search_find_next();
                self.set_state(found);
            }
            Err(_) => self.set_state(false),
        }
    }

    pub(crate) fn step(&self, terminal: Option<vte4::Terminal>, forward: bool) {
        let Some(t) = terminal else {
            return;
        };
        if self.entry.text().is_empty() {
            return;
        }
        let found = if forward {
            t.search_find_next()
        } else {
            t.search_find_previous()
        };
        self.set_state(found);
    }

    pub(crate) fn set_state(&self, found: bool) {
        if found {
            self.info.set_text("");
            self.info.remove_css_class("nomatch");
        } else {
            self.info.set_text("no match");
            self.info.add_css_class("nomatch");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FocusTransition;

    #[test]
    fn pane_focus_transfers_only_visible_search_state() {
        assert_eq!(
            FocusTransition::new(true, true),
            FocusTransition {
                clear_previous: true,
                update_current: true,
            }
        );
        assert_eq!(
            FocusTransition::new(true, false),
            FocusTransition {
                clear_previous: true,
                update_current: false,
            }
        );
        assert_eq!(
            FocusTransition::new(false, true),
            FocusTransition {
                clear_previous: false,
                update_current: true,
            }
        );
    }
}

// -------------------------------------------------------------------------------------------------
// Copy / scroll mode (Cmd+Shift+C) — keyboard scrollback navigation without the mouse.
//
// VTE 0.8 exposes no API to set an arbitrary text selection by cell coordinates, so a full vi visual
// selection isn't achievable without reimplementing the grid. This mode therefore focuses on what IS
// possible: keyboard scrollback navigation (j/k, Ctrl-d/u, g/G), `/` to hand off to search, and
// select-all + yank. Esc/q exits.
// -------------------------------------------------------------------------------------------------
