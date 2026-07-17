//! Product-owned composition of generic GUI components with Husklet workspace behavior.

use crate::config::WorkspaceConfig;
use hl_gui::{Element, Event, EventId, Field, FieldId, FieldKind, Settings, Value, View};

const VPN_CHANGE: &str = "workspace.vpn.changed";

/// The application boundary that applies a workspace settings change. An engine/JIT adapter implements
/// this port; `hl-gui` only emits the value.
pub trait WorkspaceChanges {
    type Error;

    fn vpn(&mut self, workspace: &str, value: &str) -> Result<(), Self::Error>;
}

pub struct WorkspaceSettings<'a> {
    workspace: &'a WorkspaceConfig,
}

impl<'a> WorkspaceSettings<'a> {
    pub fn new(workspace: &'a WorkspaceConfig) -> Self {
        Self { workspace }
    }

    pub fn view(&self) -> View {
        let vpn = self
            .workspace
            .vpn
            .as_ref()
            .map(|value| value.to_spec())
            .unwrap_or_default();
        let settings = Settings {
            title: Some("Workspace settings".into()),
            fields: vec![Field {
                id: FieldId::new("vpn"),
                label: "VPN".into(),
                help: Some("Route this workspace through a proxy or VPN".into()),
                kind: FieldKind::Text {
                    placeholder: "socks5:host:port".into(),
                    secret: false,
                },
                value: Value::Text(vpn),
                change: EventId::new(VPN_CHANGE),
                enabled: true,
            }],
            submit: None,
        };
        View::new(self.workspace.name.clone(), [Element::Settings(settings)])
    }

    pub fn apply<C: WorkspaceChanges>(
        &self,
        event: Event,
        changes: &mut C,
    ) -> Result<bool, C::Error> {
        let Event::Change {
            id,
            value: Value::Text(value),
        } = event
        else {
            return Ok(false);
        };
        if id.as_str() != VPN_CHANGE {
            return Ok(false);
        }
        changes.vpn(&self.workspace.name, &value)?;
        Ok(true)
    }
}
