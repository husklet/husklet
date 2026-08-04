use hl_gui::{Element, Event, EventId, Events, Field, FieldId, FieldKind, ListItem, Settings, Value, View};

#[derive(Default)]
struct Recorded(Vec<Event>);

impl Events for Recorded {
    fn emit(&mut self, event: Event) {
        self.0.push(event);
    }
}

#[test]
fn application_supplies_settings_and_owns_change_behavior() {
    let change = EventId::new("workspace.vpn.changed");
    let settings = Settings::new([Field {
        id: FieldId::new("vpn"),
        label: "VPN".into(),
        help: Some("Workspace egress".into()),
        kind: FieldKind::Text {
            placeholder: "socks5:host:port".into(),
            secret: false,
        },
        value: Value::Text("socks5:127.0.0.1:1080".into()),
        change: change.clone(),
        enabled: true,
    }]);
    let view = View::new("Workspace", [Element::Settings(settings)]);

    assert_eq!(view.title, "Workspace");
    let mut events = Recorded::default();
    events.emit(Event::Change {
        id: change.clone(),
        value: Value::Text("direct".into()),
    });
    assert_eq!(
        events.0,
        vec![Event::Change {
            id: change,
            value: Value::Text("direct".into())
        }]
    );
}

#[test]
fn application_composes_navigation_rows_without_gui_policy() {
    let open = EventId::new("workspace.open.alpha");
    let mut item = ListItem::new("alpha", open.clone());
    item.subtitle = Some("debian:bookworm".into());
    item.selected = true;

    let view = View::new(
        "Workspaces",
        [Element::section("Configured", [Element::list([item.clone()])])],
    );
    assert_eq!(
        view.content,
        vec![Element::Section {
            title: Some("Configured".into()),
            content: vec![Element::List(vec![item])],
        }]
    );

    let mut events = Recorded::default();
    events.emit(Event::Invoke(open.clone()));
    assert_eq!(events.0, vec![Event::Invoke(open)]);
}
