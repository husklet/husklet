use hl_gui::{Choice, Length, NodeId, Prop, PropValue, Surface, Tag, Trigger};

use super::Sample;

/// Every variant against every tone, plus the disabled state.
pub(super) fn buttons(surface: &mut Surface, parent: NodeId) {
    for variant in Sample::VARIANTS {
        let row = Sample::strip(surface, parent);
        for tone in Sample::TONES {
            let button = surface.button(tone.as_str(), Sample::event("press"));
            surface.style(button, *variant, *tone);
            surface.append(row, button);
        }
    }

    let extras = Sample::strip(surface, parent);

    let disabled = surface.button("Disabled", Sample::event("noop"));
    surface.set(disabled, Prop::Enabled, PropValue::Flag(false));
    surface.append(extras, disabled);

    let icon = surface.create(Tag::IconButton);
    surface.set(icon, Prop::Icon, PropValue::text("view-refresh-symbolic"));
    surface.on(icon, Trigger::Invoke, Sample::event("refresh"));
    surface.append(extras, icon);

    let toggle = surface.create(Tag::ToggleButton);
    surface.set(toggle, Prop::Label, PropValue::text("Follow logs"));
    surface.set(toggle, Prop::Checked, PropValue::Flag(true));
    surface.on(toggle, Trigger::Toggle, Sample::event("follow"));
    surface.append(extras, toggle);
}

/// Text-entry family.
pub(super) fn inputs(surface: &mut Surface, parent: NodeId) {
    let entry = surface.entry("husklet-workspace", Sample::event("name"));
    surface.set(entry, Prop::Placeholder, PropValue::text("Container name"));
    surface.set(entry, Prop::Width, PropValue::Length(Length::Fill));
    let named = Sample::labelled(surface, parent, "Entry");
    surface.append(named, entry);

    let secret = surface.entry("hunter2", Sample::event("token"));
    surface.set(secret, Prop::Secret, PropValue::Flag(true));
    let hidden = Sample::labelled(surface, parent, "Secret entry");
    surface.append(hidden, secret);

    let search = surface.create(Tag::Search);
    surface.set(search, Prop::Placeholder, PropValue::text("Filter images…"));
    surface.on(search, Trigger::Change, Sample::event("filter"));
    let found = Sample::labelled(surface, parent, "Search");
    surface.append(found, search);

    let number = surface.create(Tag::NumberEntry);
    surface.set(number, Prop::Minimum, PropValue::Number(1.0));
    surface.set(number, Prop::Maximum, PropValue::Number(64.0));
    surface.set(number, Prop::Value, PropValue::Number(4.0));
    let counted = Sample::labelled(surface, parent, "Number");
    surface.append(counted, number);

    let area = surface.create(Tag::TextArea);
    surface.set(
        area,
        Prop::Value,
        PropValue::text("FROM alpine:3.20\nRUN apk add --no-cache curl\nCMD [\"/bin/sh\"]"),
    );
    surface.set(area, Prop::Monospace, PropValue::Flag(true));
    let edited = Sample::labelled(surface, parent, "Text area");
    surface.append(edited, area);
}

/// Controls that pick from a fixed set.
pub(super) fn selection(surface: &mut Surface, parent: NodeId) {
    let toggles = Sample::strip(surface, parent);

    let switch = surface.create(Tag::Switch);
    surface.set(switch, Prop::Checked, PropValue::Flag(true));
    surface.on(switch, Trigger::Toggle, Sample::event("autostart"));
    surface.append(toggles, switch);

    let check = surface.create(Tag::Checkbox);
    surface.set(check, Prop::Label, PropValue::text("Remove volumes"));
    surface.on(check, Trigger::Toggle, Sample::event("volumes"));
    surface.append(toggles, check);

    let radio = surface.create(Tag::RadioGroup);
    surface.set(radio, Prop::Choices, choices());
    let picked = Sample::labelled(surface, parent, "Radio group");
    surface.append(picked, radio);

    let select = surface.create(Tag::Select);
    surface.set(select, Prop::Choices, choices());
    let chosen = Sample::labelled(surface, parent, "Dropdown");
    surface.append(chosen, select);

    let slider = surface.create(Tag::Slider);
    surface.set(slider, Prop::Minimum, PropValue::Number(0.0));
    surface.set(slider, Prop::Maximum, PropValue::Number(100.0));
    surface.set(slider, Prop::Value, PropValue::Number(35.0));
    surface.on(slider, Trigger::Change, Sample::event("memory"));
    let ranged = Sample::labelled(surface, parent, "Slider");
    surface.append(ranged, slider);
}

fn choices() -> PropValue {
    PropValue::Choices(vec![
        Choice::new("always", "Always restart"),
        Choice::new("failure", "On failure"),
        Choice::new("never", "Never"),
    ])
}
