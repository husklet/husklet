use hl_gui::{Align, Length, NodeId, Orientation, Prop, PropValue, Scale, Surface, Tag, Tone};

use super::Sample;

/// Spacing scale, orientation, and dividers.
pub(super) fn spacing(surface: &mut Surface, parent: NodeId) {
    for step in [1_u8, 2, 4, 6] {
        let labelled = Sample::labelled(surface, parent, &format!("gap {step}"));
        let row = surface.container(Tag::Row, Length::Step(step));
        surface.append(labelled, row);
        for index in 0..4 {
            let chip = surface.badge(format!("item {index}"), Tone::Accent);
            surface.append(row, chip);
        }
    }

    let divider = surface.create(Tag::Separator);
    surface.set(divider, Prop::Width, PropValue::Length(Length::Fill));
    surface.append(parent, divider);

    let split = surface.create(Tag::Splitter);
    surface.set(split, Prop::Position, PropValue::Number(180.0));
    surface.set(split, Prop::Height, PropValue::Length(Length::Step(12)));
    surface.append(parent, split);
    for side in ["left pane", "right pane"] {
        let column = surface.container(Tag::Column, Length::Step(2));
        surface.set(column, Prop::Pad, PropValue::Length(Length::Step(2)));
        surface.append(split, column);
        let text = surface.text(side);
        surface.append(column, text);
    }

    let vertical = surface.container(Tag::Row, Length::Step(2));
    surface.set(
        vertical,
        Prop::Orientation,
        PropValue::Orientation(Orientation::Vertical),
    );
    surface.append(parent, vertical);
    let note = surface.text("A Row re-oriented vertically is the same component.");
    surface.set(note, Prop::Scale, PropValue::Scale(Scale::Caption));
    surface.append(vertical, note);
}

/// Framing chrome: cards, toolbars, expanders, notices, tabs.
pub(super) fn surfaces(surface: &mut Surface, parent: NodeId) {
    let toolbar = surface.create(Tag::Toolbar);
    surface.set(toolbar, Prop::Pad, PropValue::Length(Length::Step(2)));
    surface.append(parent, toolbar);
    let title = surface.text("workspace / containers");
    surface.append(toolbar, title);
    let spacer = surface.create(Tag::Spacer);
    surface.append(toolbar, spacer);
    let action = surface.button("Prune", Sample::event("prune"));
    surface.style(action, hl_gui::Variant::Outline, Tone::Danger);
    surface.append(toolbar, action);

    let banner = surface.create(Tag::Banner);
    surface.set(banner, Prop::Expanded, PropValue::Flag(true));
    surface.append(parent, banner);
    let notice = surface.text("Extension stopped responding — showing the last known state.");
    surface.append(banner, notice);

    let expander = surface.create(Tag::Expander);
    surface.set(expander, Prop::Label, PropValue::text("Advanced options"));
    surface.set(expander, Prop::Expanded, PropValue::Flag(true));
    surface.append(parent, expander);
    let inner = surface.container(Tag::Column, Length::Step(2));
    surface.set(inner, Prop::Pad, PropValue::Length(Length::Step(2)));
    surface.append(expander, inner);
    let hint = surface.text("Nested content is retained, not rebuilt, when it collapses.");
    surface.set(hint, Prop::Scale, PropValue::Scale(Scale::Caption));
    surface.append(inner, hint);

    let tabs = surface.create(Tag::Tabs);
    surface.set(tabs, Prop::Height, PropValue::Length(Length::Step(12)));
    surface.append(parent, tabs);
    for name in ["Overview", "Logs", "Settings"] {
        let page = surface.create(Tag::TabPage);
        surface.set(page, Prop::Pad, PropValue::Length(Length::Step(3)));
        surface.append(tabs, page);
        let body = surface.text(format!("{name} page content"));
        surface.set(body, Prop::Align, PropValue::Align(Align::Start));
        surface.append(page, body);
    }
}
