use hl_gui::{Length, NodeId, Prop, PropValue, Scale, Surface, Tag, Tone};

use super::Sample;

/// Text scales, monospace, and links.
pub(super) fn typography(surface: &mut Surface, parent: NodeId) {
    for scale in Scale::ALL {
        let text = surface.text(format!("{} — the quick brown fox", scale.as_str()));
        surface.set(text, Prop::Scale, PropValue::Scale(*scale));
        surface.append(parent, text);
    }

    let code = surface.create(Tag::Code);
    surface.set(
        code,
        Prop::Label,
        PropValue::text("docker run --rm -it alpine:3.20 /bin/sh"),
    );
    surface.append(parent, code);

    let link = surface.create(Tag::Link);
    surface.set(link, Prop::Label, PropValue::text("Open documentation"));
    surface.set(link, Prop::Uri, PropValue::text("https://example.invalid"));
    surface.append(parent, link);

    let wrapped = surface.text(
        "Long descriptive copy wraps when the wrap property is set, so a card can carry an \
         explanation without forcing the window wider than the content needs.",
    );
    surface.set(wrapped, Prop::Wrap, PropValue::Flag(true));
    surface.set(wrapped, Prop::Scale, PropValue::Scale(Scale::Caption));
    surface.append(parent, wrapped);
}

/// Badges, avatar, progress, and activity.
pub(super) fn status(surface: &mut Surface, parent: NodeId) {
    let badges = Sample::strip(surface, parent);
    for tone in Sample::TONES {
        let badge = surface.badge(label(*tone), *tone);
        surface.append(badges, badge);
    }

    let indicators = Sample::strip(surface, parent);

    let avatar = surface.create(Tag::Avatar);
    surface.set(avatar, Prop::Label, PropValue::text("HL"));
    surface.append(indicators, avatar);

    let spinner = surface.create(Tag::Spinner);
    surface.set(spinner, Prop::Busy, PropValue::Flag(true));
    surface.append(indicators, spinner);

    let icon = surface.create(Tag::Icon);
    surface.set(icon, Prop::Icon, PropValue::text("folder-symbolic"));
    surface.append(indicators, icon);

    for fraction in [0.25_f64, 0.6, 1.0] {
        let progress = surface.create(Tag::Progress);
        surface.set(progress, Prop::Fraction, PropValue::Number(fraction));
        surface.set(progress, Prop::Width, PropValue::Length(Length::Fill));
        surface.append(parent, progress);
    }
}

const fn label(tone: Tone) -> &'static str {
    match tone {
        Tone::Neutral => "created",
        Tone::Accent => "pulling",
        Tone::Positive => "running",
        Tone::Warning => "restarting",
        Tone::Danger => "exited",
    }
}
