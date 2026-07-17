#![cfg(feature = "components")]

use hl::config::WorkspaceConfig;
use hl::presentation::{WorkspaceChanges, WorkspaceSettings};
use hl_gui::{Event, EventId, Value};
use hl_ws::Arch;

#[derive(Default)]
struct Changes(Vec<(String, String)>);

impl WorkspaceChanges for Changes {
    type Error = std::convert::Infallible;

    fn vpn(&mut self, workspace: &str, value: &str) -> Result<(), Self::Error> {
        self.0.push((workspace.into(), value.into()));
        Ok(())
    }
}

#[test]
fn vpn_input_is_generic_but_husklet_owns_its_effect() {
    let workspace = WorkspaceConfig::new("dev", "ubuntu", Arch::Arm64);
    let settings = WorkspaceSettings::new(&workspace);
    let mut changes = Changes::default();

    let applied = settings
        .apply(
            Event::Change {
                id: EventId::new("workspace.vpn.changed"),
                value: Value::Text("socks5:127.0.0.1:1080".into()),
            },
            &mut changes,
        )
        .unwrap();

    assert!(applied);
    assert_eq!(
        changes.0,
        vec![("dev".into(), "socks5:127.0.0.1:1080".into())]
    );
}
