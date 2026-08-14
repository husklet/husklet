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
        "button {{ background: {raised}; color: {text}; border: 1px solid {line}; border-radius: {radius}px; }}\n\
         button:hover {{ background: {line}; }}\n\
         button:disabled {{ color: {faint}; background: {surface}; }}\n\
         entry, spinbutton, textview, textview text, dropdown, dropdown > button, calendar {{ \
           background: {ground}; color: {text}; border: 1px solid {line}; border-radius: {radius}px; }}\n\
         entry text, spinbutton text {{ color: {text}; }}\n\
         entry:focus-within, textview:focus-within {{ border-color: {accent}; }}\n\
         scrolledwindow, viewport, listview, columnview, notebook, frame, paned, expander {{ \
           background: transparent; color: {text}; }}\n\
         notebook header, notebook tab {{ background: {surface}; color: {dim}; }}\n\
         notebook tab:checked {{ color: {text}; }}\n\
         switch {{ background: {line}; }}\n\
         switch:checked {{ background: {accent}; }}\n\
         scale trough {{ background: {line}; }}\n\
         scale highlight {{ background: {accent}; }}\n\
         progressbar trough {{ background: {line}; }}\n\
         progressbar progress {{ background: {accent}; }}\n\
         checkbutton check {{ background: {ground}; border: 1px solid {line}; }}\n\
         checkbutton check:checked {{ background: {accent}; }}\n\
         label {{ color: inherit; }}",
        ground = theme.color(Token::Ground).hex(),
        surface = theme.color(Token::Surface).hex(),
        raised = theme.color(Token::Raised).hex(),
        line = theme.color(Token::Line).hex(),
        text = theme.color(Token::Text).hex(),
        dim = theme.color(Token::TextDim).hex(),
        faint = theme.color(Token::TextFaint).hex(),
        accent = theme.color(Token::Accent).hex(),
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
         .hl-card {{ background: {surface}; border: 1px solid {line}; border-radius: {radius}px; }}\n\
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
            ".tone-{name} {{ color: {color}; }}\n\
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
}

fn scales(css: &mut String, theme: &Theme) {
    let steps = [
        (Scale::Caption, 11, "400", Token::TextDim),
        (Scale::Body, 13, "400", Token::Text),
        (Scale::Title, 16, "600", Token::Text),
        (Scale::Display, 22, "700", Token::Text),
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
            ".gap-{step} {{ padding: 0; margin: 0; }}\n.pad-{step} {{ padding: {pixels}px; }}\n"
        );
    }
}

fn components(css: &mut String, theme: &Theme) {
    let radius = theme.radius.pixels().unwrap_or(4);
    let _ = writeln!(
        css,
        ".hl-badge {{ background: {raised}; color: {dim}; border-radius: {pill}px; padding: 1px 8px; font-size: 11px; }}\n\
         .hl-avatar {{ background: {accent}; color: {ground}; border-radius: 18px; font-weight: 700; }}\n\
         .hl-banner, .hl-toast {{ background: {raised}; border: 1px solid {line}; border-radius: {radius}px; padding: 8px 12px; }}\n\
         .hl-separator {{ background: {line}; min-height: 1px; min-width: 1px; }}\n\
         .hl-datatable, .hl-list {{ background: {surface}; border: 1px solid {line}; border-radius: {radius}px; }}\n\
         .hl-heading {{ font-size: 16px; font-weight: 600; }}\n\
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
    let (prefix, name) = match (prop, value) {
        (Prop::Variant, PropValue::Variant(variant)) => ("variant", variant.as_str()),
        (Prop::Tone, PropValue::Tone(tone)) => ("tone", tone.as_str()),
        (Prop::Scale, PropValue::Scale(scale)) => ("scale", scale.as_str()),
        (Prop::Color, PropValue::Token(token)) => ("tone", token.as_str()),
        _ => return,
    };
    for existing in widget.css_classes() {
        if existing.starts_with(&format!("{prefix}-")) {
            widget.remove_css_class(&existing);
        }
    }
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
    }
}
