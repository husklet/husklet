pub(crate) struct WorkspaceFeatureFields {
    pub(crate) docker: gtk::Switch,
    pub(crate) vpn: gtk::Entry,
}

impl WorkspaceFeatureFields {
    pub(crate) fn new() -> Self {
        Self {
            docker: gtk::Switch::new(),
            vpn: Field::entry("socks5:127.30.0.1:1080  (blank = direct)", true),
        }
    }

    pub(crate) fn docker(&self) -> gtk::Box {
        Field::toggle(
            "Docker socket",
            "Make the host Docker service available inside the workspace.",
            &self.docker,
        )
    }

    pub(crate) fn network(&self) -> gtk::Box {
        self.vpn.set_hexpand(true);
        Field::text(
            "VPN OR PROXY",
            &self.vpn,
            Some("Blank uses the host's direct connection."),
        )
    }
}

// =================================================================================================
// Window 2 — New Workspace (settings sheet)
// =================================================================================================

use crate::components::layout::Field;
/// Handles to every field, so Create can gather the full config.
use crate::*;
