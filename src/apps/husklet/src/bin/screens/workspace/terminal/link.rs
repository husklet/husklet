//! Links in a pane: matching them, hovering them, and opening them.

use super::*;

impl Terminal<'_> {
    pub(crate) fn setup_hyperlinks(&self) {
        let term = self.0;
        let flags = PCRE2_UTF | PCRE2_NO_UTF_CHECK | PCRE2_MULTILINE | PCRE2_UCP | PCRE2_CASELESS;
        if let Ok(re) = vte4::Regex::for_match(URL_REGEX, flags) {
            let tag = term.match_add_regex(&re, 0);
            term.match_set_cursor_name(tag, "pointer");
        }
        term.set_mouse_autohide(true);

        // Hover cue: reflect the hovered OSC-8 link in the tooltip.
        term.connect_hyperlink_hover_uri_notify(|t| {
            let uri = t.hyperlink_hover_uri();
            t.set_tooltip_text(uri.as_deref());
        });

        // Click-to-open. Primary click over a URL match opens it; a modifier (Cmd/Ctrl) always opens the link
        // under the pointer even for explicit OSC-8 links.
        let click = gtk::GestureClick::new();
        click.set_button(1); // primary only
        let t = term.clone();
        click.connect_released(move |g, _n, x, y| {
            // Cmd/Ctrl-click opens the link under the pointer (an explicit OSC-8 hyperlink, else a regex URL
            // match). A modifier is required so a plain click / text selection is never hijacked.
            let state = g.current_event_state();
            let modified =
                state.contains(gdk::ModifierType::META_MASK) || state.contains(gdk::ModifierType::CONTROL_MASK);
            if !modified {
                return;
            }
            let uri = t.hyperlink_hover_uri().or_else(|| t.check_match_at(x, y).0);
            if let Some(uri) = uri {
                Url::new(&uri).open();
            }
        });
        term.add_controller(click);
    }
}

/// A normalized URL opened by the desktop's registered handler.
pub(crate) struct Url(String);

impl Url {
    pub(crate) fn new(url: &str) -> Self {
        Self(if url.starts_with("www.") {
            format!("https://{url}")
        } else {
            url.to_string()
        })
    }

    pub(crate) fn open(&self) {
        let _ = crate::gtk_adapter::Uri::new(&self.0).open();
    }
}
