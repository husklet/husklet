//! Theme to stylesheet. Generated once, applied by class, never per widget.

use std::fmt::Write as _;

use gtk::prelude::*;
use hl_gui::{Density, Length, Prop, PropValue, Scale, Theme, Token, Tone, Variant};

/// Emits the full class sheet for a theme.
///
/// Every enumerated value gets one rule, so styling a node is adding a class
/// rather than attaching a provider — the difference between constant-time
/// restyling and a per-widget style cascade.
#[must_use]
pub fn sheet(theme: &Theme) -> String {
    let mut css = String::with_capacity(8192);
    base(&mut css, theme);
    controls(&mut css, theme);
    tones(&mut css, theme);
    variants(&mut css, theme);
    scales(&mut css, theme);
    control_sizes(&mut css);
    spacing(&mut css, theme);
    components(&mut css, theme);
    css
}

/// Control chrome. Without these the toolkit's own defaults show through a
/// dark theme, so the palette must reach the widgets the sheet does not name.
fn controls(css: &mut String, theme: &Theme) {
    let radius = theme.radius.pixels().unwrap_or(4);
    let _ = writeln!(
        css,
        "button {{ background: transparent; color: {text}; border: 1px solid transparent; border-radius: {radius}px; min-height: 28px; min-width: 28px; padding: 3px 10px; font-size: 13px; font-weight: 500; box-shadow: none; transition: 120ms ease; }}\n\
         button:hover {{ background: {raised}; border-color: {line}; }}\n\
         button:active {{ background: {surface}; box-shadow: inset 0 1px 2px rgba(0,0,0,.24); }}\n\
         button:focus-visible {{ outline: 2px solid {accent}; outline-offset: 2px; }}\n\
         button:disabled {{ color: {faint}; background: {surface}; border-color: {surface}; box-shadow: none; }}\n\
         entry, spinbutton, textview, textview text, dropdown, dropdown > button, calendar {{ \
           background: {ground}; color: {text}; border: 1px solid {line}; border-radius: {radius}px; min-height: 30px; }}\n\
         entry text, spinbutton text {{ color: {text}; }}\n\
         entry, spinbutton, textview, dropdown > button {{ padding: 3px 9px; }}\n\
         .hl-select {{ background: {ground}; border: 1px solid {line}; border-radius: {radius}px; min-height: 30px; }}\n\
         .hl-select:hover {{ border-color: {dim}; }}\n\
         .hl-select:focus-within {{ border-color: {accent}; box-shadow: 0 0 0 1px {accent}; }}\n\
         .hl-select:disabled, .hl-select button:disabled {{ border-color: {faint}; }}\n\
         entry:hover, spinbutton:hover, dropdown:hover > button {{ border-color: {dim}; }}\n\
         entry:focus-within, spinbutton:focus-within, textview:focus-within {{ border-color: {accent}; box-shadow: 0 0 0 1px {accent}; }}\n\
         entry.tone-danger, .hl-entry.tone-danger {{ border-color: {danger}; box-shadow: 0 0 0 1px {danger}; }}\n\
         scrolledwindow, viewport, listview, columnview, notebook, frame, paned, expander {{ \
           background: transparent; color: {text}; }}\n\
         notebook header, notebook tab {{ background: {surface}; color: {dim}; }}\n\
         notebook tab:checked {{ color: {text}; }}\n\
         switch {{ background: {line}; }}\n\
         switch:checked {{ background: {accent}; }}\n\
         switch:focus, switch:focus-visible {{ outline: 2px solid {accent}; outline-offset: 2px; box-shadow: 0 0 0 3px {ground}; }}\n\
         switch:disabled {{ opacity: .55; }}\n\
         scale trough {{ background: {line}; }}\n\
         scale highlight {{ background: {accent}; }}\n\
         scale:focus slider, scale:focus-visible slider {{ outline: 2px solid {accent}; outline-offset: 2px; }}\n\
         scale:disabled {{ opacity: .55; }}\n\
         progressbar trough {{ background: {line}; }}\n\
         progressbar progress {{ background: {accent}; }}\n\
         checkbutton check {{ background: {ground}; border: 1px solid {line}; }}\n\
         checkbutton check:checked {{ background: {accent}; }}\n\
         checkbutton:focus, checkbutton:focus-visible {{ outline: 2px solid {accent}; outline-offset: 2px; }}\n\
         .hl-table {{ background: {ground}; border: 1px solid {line}; border-radius: {radius}px; }}\n\
         .hl-tablehead .hl-tablecell {{ background: {raised}; color: {text}; font-weight: 600; }}\n\
         .hl-tablecell {{ min-height: 32px; padding: 6px 8px; border-bottom: 1px solid {line}; }}\n\
         .hl-tablebody .hl-tablerow:hover .hl-tablecell {{ background: {surface}; }}\n\
         label {{ color: inherit; }}",
        ground = theme.color(Token::Ground).hex(),
        surface = theme.color(Token::Surface).hex(),
        raised = theme.color(Token::Raised).hex(),
        line = theme.color(Token::Line).hex(),
        text = theme.color(Token::Text).hex(),
        dim = theme.color(Token::TextDim).hex(),
        faint = theme.color(Token::TextFaint).hex(),
        accent = theme.color(Token::Accent).hex(),
        danger = theme.color(Token::Danger).hex(),
        radius = radius,
    );
}

fn base(css: &mut String, theme: &Theme) {
    let radius = theme.radius.pixels().unwrap_or(4);
    let _ = writeln!(
        css,
        "window, .hl-root {{ background: {ground}; color: {text}; font-family: {font}; }}\n\
         .hl-surface {{ background: {surface}; }}\n\
         * {{ outline-color: {accent}; }}\n\
         .hl-code, .monospace {{ font-family: {mono}; }}\n\
         .hl-card {{ background: {surface}; border: 1px solid {line}; border-radius: {radius}px; box-shadow: 0 1px 2px rgba(0,0,0,.22); }}\n\
         .hl-toolbar, .hl-headerbar {{ background: {raised}; border-bottom: 1px solid {line}; }}\n\
         .hl-sidebar {{ background: {surface}; border-right: 1px solid {line}; }}",
        ground = theme.color(Token::Ground).hex(),
        surface = theme.color(Token::Surface).hex(),
        raised = theme.color(Token::Raised).hex(),
        line = theme.color(Token::Line).hex(),
        text = theme.color(Token::Text).hex(),
        accent = theme.color(Token::Accent).hex(),
        font = theme.font,
        mono = theme.monospace,
        radius = radius,
    );
}

fn tones(css: &mut String, theme: &Theme) {
    for tone in Tone::ALL {
        let color = theme.color(token(*tone)).hex();
        // Component rules and tone rules have equal specificity, so a toned
        // component needs the compound selector to win over its own default.
        let _ = writeln!(
            css,
            // A component rule and a tone rule carry equal weight, so whichever
            // is written last would win by accident. Repeating the class raises
            // the tone above every single-class component rule, which is what a
            // producer means when it tones a component.
            ".tone-{name}.tone-{name} {{ color: {color}; }}\n\
             .hl-badge.tone-{name} {{ color: {color}; border: 1px solid {color}; }}",
            name = tone.as_str(),
        );
    }
}

fn variants(css: &mut String, theme: &Theme) {
    let radius = theme.radius.pixels().unwrap_or(4);
    for variant in Variant::ALL {
        for tone in Tone::ALL {
            let color = theme.color(token(*tone)).hex();
            let rule = match variant {
                Variant::Filled => format!(
                    "background: {color}; color: {on}; border: none;",
                    on = theme.color(Token::Ground).hex()
                ),
                Variant::Outline => format!("background: transparent; border: 1px solid {color}; color: {color};"),
                Variant::Ghost => format!("background: transparent; border: none; color: {color};"),
                Variant::Plain => format!("color: {color};"),
            };
            let _ = writeln!(
                css,
                ".variant-{variant}.tone-{tone} {{ {rule} border-radius: {radius}px; }}",
                variant = variant.as_str(),
                tone = tone.as_str(),
            );
        }
    }
    let _ = writeln!(
        css,
        ".variant-plain {{ background: transparent; color: {text}; border-color: transparent; box-shadow: none; }}\n\
         .variant-filled {{ background: {text}; color: {ground}; border-color: {text}; }}\n\
         .variant-outline {{ background: transparent; color: {text}; border: 1px solid {line}; }}\n\
         .variant-ghost {{ background: transparent; color: {dim}; border-color: transparent; box-shadow: none; }}\n\
         .variant-outline:hover, .variant-ghost:hover {{ background: {raised}; color: {text}; border-color: {line}; }}\n\
         .variant-filled:hover {{ box-shadow: inset 0 0 0 999px rgba(255,255,255,.10); }}\n\
         .variant-filled:active {{ box-shadow: inset 0 0 0 999px rgba(0,0,0,.14); }}\n\
         .variant-filled:disabled, .variant-outline:disabled {{ background: {surface}; color: {faint}; border-color: {line}; }}\n\
         .variant-ghost:disabled, .variant-plain:disabled {{ background: transparent; color: {faint}; border-color: transparent; }}",
        raised = theme.color(Token::Raised).hex(),
        text = theme.color(Token::Text).hex(),
        ground = theme.color(Token::Ground).hex(),
        dim = theme.color(Token::TextDim).hex(),
        surface = theme.color(Token::Surface).hex(),
        faint = theme.color(Token::TextFaint).hex(),
        line = theme.color(Token::Line).hex(),
    );
}

fn scales(css: &mut String, theme: &Theme) {
    let steps = [
        (Scale::Caption, 12, "450", Token::TextDim),
        (Scale::Body, 14, "400", Token::Text),
        (Scale::Title, 18, "600", Token::Text),
        (Scale::Display, 24, "700", Token::Text),
    ];
    for (scale, size, weight, token) in steps {
        let _ = writeln!(
            css,
            ".scale-{name} {{ font-size: {size}px; font-weight: {weight}; color: {color}; }}",
            name = scale.as_str(),
            color = theme.color(token).hex(),
        );
    }
}

fn control_sizes(css: &mut String) {
    css.push_str(
        "button.size-small { min-height: 28px; padding: 4px 8px; font-size: 12px; border-radius: 6px; }\n\
         button.size-medium { min-height: 36px; padding: 8px 12px; font-size: 14px; border-radius: 6px; }\n\
         button.size-large { min-height: 44px; padding: 12px 16px; font-size: 16px; border-radius: 8px; }\n\
         button.size-small > box { border-spacing: 6px; } button.size-small image { -gtk-icon-size: 14px; }\n\
         button.size-medium > box { border-spacing: 8px; } button.size-medium image { -gtk-icon-size: 18px; }\n\
         button.size-large > box { border-spacing: 8px; } button.size-large image { -gtk-icon-size: 20px; }\n\
         button.hl-iconbutton.size-small { min-width: 28px; min-height: 28px; padding: 0; }\n\
         button.hl-iconbutton.size-medium { min-width: 36px; min-height: 36px; padding: 0; }\n\
         button.hl-iconbutton.size-large { min-width: 44px; min-height: 44px; padding: 0; }\n",
    );
}

fn spacing(css: &mut String, theme: &Theme) {
    let factor = match theme.density {
        Density::Compact => 0.75_f32,
        Density::Normal => 1.0,
        Density::Comfortable => 1.5,
    };
    for step in 0..=Length::MAXIMUM_STEP {
        let pixels = (f32::from(step) * f32::from(Length::STEP_PIXELS) * factor).round() as u16;
        let _ = writeln!(
            css,
            ".gap-{step} {{ padding: 0; margin: 0; }}\n\
             .pad-{step} {{ padding: {pixels}px; }}\n\
             .pad-top-{step} {{ padding-top: {pixels}px; }}\n\
             .pad-end-{step} {{ padding-right: {pixels}px; }}\n\
             .pad-bottom-{step} {{ padding-bottom: {pixels}px; }}\n\
             .pad-start-{step} {{ padding-left: {pixels}px; }}\n"
        );
    }
}

fn components(css: &mut String, theme: &Theme) {
    let radius = theme.radius.pixels().unwrap_or(4);
    let _ = writeln!(
        css,
        ".hl-badge {{ background: {raised}; color: {dim}; border-radius: {pill}px; padding: 2px 8px; font-size: 11px; font-weight: 600; }}\n\
         .hl-avatar {{ background: {accent}; color: {ground}; border-radius: 18px; font-weight: 700; }}\n\
         .hl-banner, .hl-toast {{ background: {raised}; border: 1px solid {line}; border-radius: {radius}px; padding: 8px 12px; }}\n\
         .hl-card {{ box-shadow: none; }}\n\
         .hl-cardactionarea:hover {{ background: {raised}; }}\n\
         .hl-card > box {{ padding: 10px; }}\n\
         .hl-cardactions {{ margin-top: 4px; }}\n\
         .hl-navigationmenu {{ padding: 2px 4px; }}\n\
         .hl-navigationmenuitem {{ background: transparent; color: {dim}; border: 0; border-radius: {radius}px; min-height: 30px; padding: 4px 8px; font-weight: 500; }}\n\
         .hl-navigationmenuitem:hover {{ background: {raised}; color: {text}; }}\n\
         .hl-navigationmenuitem:checked, .hl-navigationmenuitem:checked:hover {{ background: {raised}; color: {text}; box-shadow: inset 2px 0 0 {accent}; }}\n\
         .hl-listitembutton, .hl-listitembutton.variant-ghost {{ background: transparent; color: {dim}; border: 0; border-radius: 4px; min-height: 28px; padding: 3px 8px; box-shadow: none; font-weight: 400; }}\n\
         .hl-listitembutton:hover, .hl-listitembutton.variant-ghost:hover {{ background: {raised}; color: {text}; }}\n\
         .hl-listitembutton.variant-filled, .hl-listitembutton.variant-filled:hover {{ background: {raised}; color: {text}; box-shadow: inset 2px 0 0 {accent}; font-weight: 600; }}\n\
         .hl-iconbutton {{ min-width: 30px; min-height: 30px; padding: 3px; border-color: transparent; background: transparent; }}\n\
         .hl-iconbutton:hover {{ background: {raised}; border-color: {line}; }}\n\
         .hl-togglebutton {{ background: transparent; border-color: {line}; }}\n\
         .hl-togglebutton:checked, .hl-togglebutton:checked:hover {{ background: {raised}; color: {text}; border-color: {accent}; box-shadow: inset 0 -3px 0 {accent}; font-weight: 700; }}\n\
         .hl-togglebutton:checked:focus-visible {{ outline: 2px solid {accent}; outline-offset: 2px; box-shadow: inset 0 -3px 0 {accent}; }}\n\
         .hl-chip {{ min-height: 24px; padding: 1px 8px; border-radius: {pill}px; background: {raised}; border-color: {line}; }}\n\
         .hl-separator {{ background: {line}; min-height: 1px; min-width: 1px; }}\n\
         .hl-datatable, .hl-list {{ background: {surface}; border: 1px solid {line}; border-radius: {radius}px; }}\n\
         .hl-heading {{ font-weight: 600; letter-spacing: -0.1px; }}\n\
         .hl-text {{ color: {text}; }}\n\
         columnview header button {{ background: {raised}; color: {dim}; font-weight: 600; }}\n\
         row:selected, :selected {{ background: {accent}; color: {ground}; }}\n\
         .hl-link {{ padding: 0; }}",
        raised = theme.color(Token::Raised).hex(),
        surface = theme.color(Token::Surface).hex(),
        line = theme.color(Token::Line).hex(),
        dim = theme.color(Token::TextDim).hex(),
        text = theme.color(Token::Text).hex(),
        accent = theme.color(Token::Accent).hex(),
        ground = theme.color(Token::Ground).hex(),
        pill = radius * 3,
        radius = radius,
    );
}

const fn token(tone: Tone) -> Token {
    match tone {
        Tone::Neutral => Token::Text,
        Tone::Accent => Token::Accent,
        Tone::Positive => Token::Positive,
        Tone::Warning => Token::Warning,
        Tone::Danger => Token::Danger,
    }
}

/// Applies an appearance property by swapping the widget's class, never by
/// attaching a per-widget provider.
pub(crate) fn mark(widget: &gtk::Widget, prop: Prop, value: &PropValue) {
    let prefix = match prop {
        Prop::Variant => "variant",
        Prop::Tone | Prop::Color => "tone",
        Prop::Scale => "scale",
        Prop::Size => "size",
        _ => return,
    };
    for existing in widget.css_classes() {
        if existing.starts_with(&format!("{prefix}-")) {
            widget.remove_css_class(&existing);
        }
    }
    let name = match (prop, value) {
        (Prop::Variant, PropValue::Variant(variant)) => variant.as_str(),
        (Prop::Tone, PropValue::Tone(tone)) => tone.as_str(),
        (Prop::Scale, PropValue::Scale(scale)) => scale.as_str(),
        (Prop::Size, PropValue::ControlSize(size)) => size.as_str(),
        (Prop::Color, PropValue::Token(token)) => token.as_str(),
        _ => return,
    };
    widget.add_css_class(&format!("{prefix}-{name}"));
}

/// Installs a sheet for the whole display at application priority.
pub fn install(theme: &Theme) {
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(true);
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_data(&sheet(theme));
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

#[cfg(test)]
mod tests {
    use hl_gui::{Theme, Tone, Variant};

    #[test]
    fn the_sheet_covers_every_variant_and_tone_pair() {
        let css = super::sheet(&Theme::dark());
        for variant in Variant::ALL {
            for tone in Tone::ALL {
                let selector = format!(".variant-{}.tone-{}", variant.as_str(), tone.as_str());
                assert!(css.contains(&selector), "missing rule {selector}");
            }
        }
        assert!(
            css.contains(".variant-outline:disabled") && css.contains("color: #87909f; border-color: #323843"),
            "semantic variants must not override disabled affordance"
        );
    }

    #[test]
    fn heading_component_chrome_does_not_flatten_the_type_scale() {
        let css = super::sheet(&Theme::dark());
        assert!(css.contains(".scale-body { font-size: 14px;"));
        assert!(css.contains(".scale-title { font-size: 18px;"));
        assert!(css.contains(".scale-display { font-size: 24px;"));
        assert!(css.contains(".hl-heading { font-weight: 600;"));
        assert!(
            !css.contains(".hl-heading { font-size:"),
            "the later component rule would override every heading scale"
        );
    }

    #[test]
    fn product_components_have_compact_distinct_chrome() {
        let css = super::sheet(&Theme::dark());
        assert!(css.contains("button:focus-visible { outline: 2px"));
        assert!(css.contains(".hl-navigationmenuitem { background: transparent"));
        assert!(css.contains(
            ".hl-navigationmenuitem:checked, .hl-navigationmenuitem:checked:hover { background: #21252d; color: #f0f2f5; box-shadow: inset 2px 0 0 #559df7"
        ));
        assert!(css.contains(".variant-ghost { background: transparent;"));
        assert!(css.contains(".hl-iconbutton { min-width: 30px; min-height: 30px;"));
        assert!(css.contains(".hl-togglebutton { background: transparent; border-color: #323843;"));
        assert!(css.contains(
            ".hl-togglebutton:checked, .hl-togglebutton:checked:hover { background: #21252d; color: #f0f2f5; border-color: #559df7; box-shadow: inset 0 -3px 0 #559df7; font-weight: 700;"
        ));
        assert!(
            css.contains(".hl-togglebutton:checked:focus-visible { outline: 2px solid #559df7; outline-offset: 2px;")
        );
        assert!(css.contains(".hl-listitembutton, .hl-listitembutton.variant-ghost { background: transparent;"));
        assert!(css.contains(".hl-card > box { padding: 10px"));
    }

    #[test]
    fn button_sizes_have_exact_independent_control_metrics() {
        let css = super::sheet(&Theme::dark());
        assert!(
            css.contains(
                "button.size-small { min-height: 28px; padding: 4px 8px; font-size: 12px; border-radius: 6px;"
            )
        );
        assert!(css.contains(
            "button.size-medium { min-height: 36px; padding: 8px 12px; font-size: 14px; border-radius: 6px;"
        ));
        assert!(css.contains(
            "button.size-large { min-height: 44px; padding: 12px 16px; font-size: 16px; border-radius: 8px;"
        ));
        assert!(css.contains("button.hl-iconbutton.size-large { min-width: 44px; min-height: 44px; padding: 0;"));
    }

    #[test]
    fn form_controls_expose_boundaries_focus_and_validation() {
        let css = super::sheet(&Theme::dark());
        assert!(css.contains(
            "switch:focus, switch:focus-visible { outline: 2px solid #559df7; outline-offset: 2px; box-shadow: 0 0 0 3px #0f1115;"
        ));
        assert!(css.contains(".hl-select { background: #0f1115; border: 1px solid #323843;"));
        assert!(css.contains(".hl-select:focus-within { border-color: #559df7; box-shadow: 0 0 0 1px #559df7;"));
        assert!(css.contains(".hl-select:disabled, .hl-select button:disabled { border-color: #87909f;"));
        assert!(css.contains("switch:disabled { opacity: .55;"));
        assert!(css.contains(
            "scale:focus slider, scale:focus-visible slider { outline: 2px solid #559df7; outline-offset: 2px;"
        ));
        assert!(css.contains("scale:disabled { opacity: .55;"));
        assert!(css.contains(
            "checkbutton:focus, checkbutton:focus-visible { outline: 2px solid #559df7; outline-offset: 2px;"
        ));
        assert!(css.contains(
            "entry.tone-danger, .hl-entry.tone-danger { border-color: #e55353; box-shadow: 0 0 0 1px #e55353;"
        ));
        assert!(css.contains(".hl-table { background: #0f1115; border: 1px solid #323843;"));
        assert!(css.contains(".hl-tablehead .hl-tablecell { background: #21252d;"));
        assert!(css.contains(".hl-tablecell { min-height: 32px; padding: 6px 8px; border-bottom: 1px solid #323843;"));
    }
}
