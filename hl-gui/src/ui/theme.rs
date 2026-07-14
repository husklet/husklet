#![allow(dead_code)]
use gtk::prelude::*;

// ---- styling ---------------------------------------------------------------

/// Flat, simple, macOS-leaning CSS: gray window, a single floating base-color pane inset with a
/// border radius ("pane in pane"), faintly tinted sidebar, no gradients. Uses theme color names so
/// light/dark both work.
pub(crate) const CSS: &str = "
/* === dd design tokens =============================================================================
   A sharp, precise developer-tool aesthetic from ONE kit: a single 6px radius on every card/control/
   popover/dialog, one hairline border token (@hl_line), an 8px spacing grid, a tight type scale, and a
   single blue accent. Theme colors keep
   light/dark working; the few literal colors are the macOS system palette. */
@define-color hl_accent #0a84ff;
@define-color hl_accent_hi #3a9bff;
@define-color hl_green #2bd158;
@define-color hl_red #ff453a;
@define-color hl_amber #ff9f0a;
@define-color hl_line alpha(@borders, 0.85);
@define-color hl_line_soft alpha(@borders, 0.5);
@define-color hl_fill alpha(@theme_fg_color, 0.06);
@define-color hl_fill_hi alpha(@theme_fg_color, 0.11);
/* Text ramp — controlled by COLOR, not opacity, so secondary text stays crisp and readable. */
@define-color hl_text @theme_fg_color;
@define-color hl_text_dim mix(@theme_fg_color, @theme_base_color, 0.32);
@define-color hl_text_faint mix(@theme_fg_color, @theme_base_color, 0.50);

window {
  background-color: mix(@theme_bg_color, @theme_base_color, 0.5);
  font-family: 'SF Pro Text', 'SF Pro Display', 'Inter', 'Helvetica Neue', -apple-system, sans-serif;
  font-size: 13.5px;
  color: @hl_text;
}
/* Secondary text everywhere reads from one crisp dim color (no more washed-out 0.5 opacity). */
.dim-label, .caption { opacity: 1; color: @hl_text_dim; }
/* Bigger headings use the Display cut for a more premium feel. */
.hl-h1, .hl-bigtitle, .title-1, .title-2, .hl-detail-title, .hl-stat-value {
  font-family: 'SF Pro Display', 'SF Pro Text', 'Inter', sans-serif;
}
.hl-topstrip { background: transparent; }

/* Surfaces: crisp 6px cards, hairline borders, no shadow. */
.hl-content {
  background-color: @theme_base_color;
  border: 1px solid @hl_line;
  border-radius: 6px;
}
.hl-sidebar {
  background-color: alpha(@theme_base_color, 0.45);
  border: 1px solid @hl_line;
  border-radius: 6px;
}

/* The paned handle is just the 8px gap between the two cards. */
paned > separator {
  background-color: transparent; background-image: none; border: none; box-shadow: none; min-width: 8px;
}

/* BOTH sidebars (nav + master list) share these rules → identical padding/rhythm/selection. */
list.navigation-sidebar { background: transparent; padding: 6px 6px; }
list.navigation-sidebar > row {
  border-radius: 6px; margin: 1px 4px; padding: 6px 8px; font-weight: 500;
}
/* Selected row reads as a SELECTION (accent tint + accent text/icon), not a neutral gray button. */
list.navigation-sidebar > row:selected { background-color: alpha(@hl_accent, 0.13); color: @hl_accent; }
list.navigation-sidebar > row:selected label { color: @hl_accent; }
list.navigation-sidebar > row:selected image { color: @hl_accent; opacity: 1; }
list.navigation-sidebar > row:hover:not(:selected) { background-color: @hl_fill; }
list.navigation-sidebar image { opacity: 0.6; }
/* No inset/active shadow or focus ring on rows — kills the inner-shadow flash on click. */
list.navigation-sidebar > row:active { background-color: alpha(@hl_accent, 0.13); }
list.navigation-sidebar > row, list.navigation-sidebar > row:active, list.navigation-sidebar > row:focus {
  box-shadow: none; outline: none;
}
row:focus, row:focus-visible, button:focus, button:focus-visible { outline: none; }
row:active, button:active { box-shadow: none; }
/* ⌘-batch members: an accent left-bar + tint, distinct from the lighter single (view) selection. */
list.navigation-sidebar > row.hl-batch { background-color: alpha(@hl_accent, 0.20); box-shadow: inset 3px 0 0 0 @hl_accent; }

/* Header status (daemon | docker): flat clickable text, no chrome. */
.hl-statusgroup { background: none; border: none; padding: 0; }
.hl-seg {
  background: none; background-color: transparent; border: none; box-shadow: none; outline: none;
  border-radius: 6px; padding: 2px 9px; min-height: 0; font-weight: 500;
}
button.hl-seg:hover { background-color: @hl_fill; }
.hl-seg.hl-active { color: @hl_green; font-weight: 700; }
menubutton.hl-seg, menubutton.hl-seg > button {
  background: none; background-color: transparent; border: none; box-shadow: none; outline: none; min-height: 0;
}
menubutton.hl-seg > button { border-radius: 6px; padding: 3px 8px; font-weight: 500; }
menubutton.hl-seg > button:hover { background-color: @hl_fill; }

/* Status dot. */
.hl-dot { min-width: 8px; min-height: 8px; border-radius: 50%; background-color: alpha(@theme_fg_color, 0.35); }
.hl-dot.success { background-color: @hl_green; }
.hl-dot.error { background-color: @hl_red; }
.hl-dot.warn { background-color: @hl_amber; }

/* Buttons: flat neutral fill. Kill Adwaita gradient background-image (the source of the gray hover
   and the raised, button-like look) on every button state up front. */
button { box-shadow: none; background-image: none; }
button:hover, button:active, button:checked, button:focus { background-image: none; }
button.flat { padding: 4px 7px; border-radius: 6px; background-color: transparent; }
.hl-btn {
  padding: 5px 14px; min-height: 0; font-weight: 600; border: none; box-shadow: none;
  border-radius: 6px; background-color: @hl_fill; background-image: none;
}
.hl-btn:hover { background-color: @hl_fill_hi; background-image: none; }
.hl-btn.suggested-action { background-color: @hl_accent; color: #ffffff; }
.hl-btn.suggested-action:hover { background-color: @hl_accent_hi; }
/* Destructive: our own class (NOT Adwaita's `.destructive-action`, which the theme would override).
   A tinted red outline by default, going solid on hover — deletes read clearly but don't shout. */
.hl-btn.hl-danger { background-color: alpha(@hl_red, 0.12); color: @hl_red; box-shadow: inset 0 0 0 1px alpha(@hl_red, 0.4); }
.hl-btn.hl-danger:hover { background-color: @hl_red; color: #ffffff; box-shadow: none; }

/* Context popover. */
popover.hl-pop { background: transparent; }
popover.hl-pop > arrow { background: transparent; border: none; }
popover.hl-pop > contents { padding: 4px; border-radius: 6px; background-color: @theme_base_color; border: 1px solid @hl_line; }
.hl-popitem {
  background: none; background-color: transparent; border: none; box-shadow: none; outline: none;
  min-height: 0; border-radius: 6px; padding: 5px 10px; font-weight: 400;
}
.hl-popitem:hover { background-color: @hl_fill; }
.hl-popitem.hl-active { color: @hl_accent; font-weight: 600; }

/* Modal dialogs: a solid, clean panel (transparency caused overlap/artefacts on macOS). The window
   carries the app's surface color; the inner box just adds padding. */
window.hl-modal { background-color: @theme_base_color; }
window.hl-modal decoration { background: transparent; box-shadow: none; }
.hl-dialog { background-color: @theme_base_color; }

.hl-update { background-color: @hl_accent; color: #ffffff; border: none; box-shadow: none; border-radius: 6px; padding: 3px 11px; font-weight: 600; min-height: 0; }
.hl-update:hover { background-color: @hl_accent_hi; }

/* Headings + dashboard. */
.hl-home { background: transparent; }
.hl-h1 { font-size: 21px; font-weight: 800; letter-spacing: -0.01em; margin-bottom: 2px; }
.hl-h2 { font-size: 15px; font-weight: 700; margin-top: 6px; }
.hl-stat-card { background-color: @theme_base_color; border: 1px solid @hl_line; border-radius: 6px; padding: 13px 15px; }
.hl-stat-value { font-size: 27px; font-weight: 800; letter-spacing: -0.02em; }
.hl-stat-value.accent { color: @hl_green; }
.hl-stat-name { font-size: 11.5px; font-weight: 600; color: @hl_text_dim; letter-spacing: 0.04em; }
.hl-update-card { background-color: alpha(@hl_accent, 0.10); border: 1px solid alpha(@hl_accent, 0.30); border-radius: 6px; padding: 13px 15px; }
.hl-mono { font-family: 'SF Mono', 'Menlo', monospace; font-size: 12px; }
.hl-step-card { background-color: @theme_base_color; border: 1px solid @hl_line; border-radius: 6px; padding: 11px 15px; }
.hl-code {
  font-family: 'SF Mono', 'Menlo', monospace; font-size: 12px; color: alpha(@theme_fg_color, 0.85);
  background-color: @hl_fill; border: 1px solid @hl_line; border-radius: 6px; padding: 10px 12px;
}

/* Onboarding. */
.hl-bigtitle { font-size: 33px; font-weight: 800; letter-spacing: -0.02em; }
.hl-sub { font-size: 13.5px; color: @hl_text_dim; }
.hl-onboard-status { font-size: 13.5px; font-weight: 600; }
.hl-onboard-head { font-size: 15px; font-weight: 700; }
.hl-cli-msg { font-family: 'SF Mono', 'Menlo', monospace; font-size: 11px; background-color: @hl_fill; border-radius: 6px; padding: 8px 10px; margin-top: 6px; }

/* Logs pane. */
.hl-logs, .hl-logs text { background-color: alpha(@theme_fg_color, 0.035); font-family: 'SF Mono', 'Menlo', monospace; font-size: 12px; }

/* Section label (uppercase, tracked). */
.hl-section-title { font-size: 0.72em; font-weight: 700; color: @hl_text_faint; letter-spacing: 0.07em; }

/* === reusable widgets (used across the resource panels) ============================================ */
/* Detail/list row: a selectable line in a master list. */
.hl-listrow { padding: 7px 10px; border-radius: 6px; }
.hl-listrow:hover { background-color: @hl_fill; }
.hl-listrow-title { font-weight: 600; font-size: 13px; }
.hl-listrow-sub { font-size: 12px; color: @hl_text_dim; }

/* Key/value detail line. */
.hl-kv-key { font-size: 12.5px; color: @hl_text_dim; min-width: 96px; }
.hl-kv-val { font-size: 13px; }

/* Status badge / chip. */
.hl-badge { font-size: 10.5px; font-weight: 700; letter-spacing: 0.03em; padding: 1px 7px; border-radius: 6px; background-color: @hl_fill; }
.hl-badge.run { color: @hl_green; background-color: alpha(@hl_green, 0.14); }
.hl-badge.stop { color: alpha(@theme_fg_color, 0.6); }
.hl-badge.fail { color: @hl_red; background-color: alpha(@hl_red, 0.13); }

/* Detail pane title + toolbar. */
.hl-detail-title { font-size: 17px; font-weight: 800; letter-spacing: -0.01em; }
.hl-detail-sub { font-size: 12.5px; color: @hl_text_dim; }
.hl-toolbar { padding: 0; }

/* Hairline separators between sections. */
.hl-hsep { background-color: @hl_line_soft; min-height: 1px; }
.hl-empty { font-size: 13px; color: @hl_text_faint; }

/* Terminal/Logs dock + System pages: flat underline tabs. EVERY state keeps identical geometry —
   same padding, same constant font-weight, an always-2px bottom border whose COLOR is the only thing
   that changes — so selecting a tab never shifts the layout. No fill, no box: not a button. */
notebook.hl-termbook { background: transparent; border: none; box-shadow: none; padding: 0; }
notebook.hl-termbook > stack { background: transparent; border: none; }
.hl-termbook > header.top {
  background: transparent; border-bottom: 1px solid @hl_line_soft; padding: 0 6px; margin: 0;
}
.hl-termbook > header.top > tabs { margin-bottom: -1px; }
.hl-termbook tab {
  padding: 7px 13px; margin: 0; min-height: 0;
  background: none; background-color: transparent; background-image: none;
  border: none; border-bottom: 2px solid transparent; box-shadow: none; outline: none; border-radius: 0;
  font-size: 12px; font-weight: 500; color: alpha(@theme_fg_color, 0.5);
}
.hl-termbook tab:hover { background: none; color: alpha(@theme_fg_color, 0.85); }
.hl-termbook tab:checked { background: none; color: @theme_fg_color; border-bottom-color: @hl_accent; }
.hl-termbook tab label { color: inherit; font-weight: inherit; }
.hl-tabclose { font-size: 11px; opacity: 0.45; padding: 0 1px; }
.hl-tabclose:hover { opacity: 1; background-color: @hl_fill; }
";

pub(crate) fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
