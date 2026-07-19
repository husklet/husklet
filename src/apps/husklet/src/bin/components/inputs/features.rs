pub(crate) struct WorkspaceFeatureFields {
    pub(crate) docker: gtk::Switch,
    pub(crate) graphical: gtk::Switch,
    pub(crate) vpn: gtk::Entry,
    pub(crate) cuda: gtk::Switch,
    pub(crate) cuda_name: gtk::Entry,
    pub(crate) cuda_capability: gtk::Entry,
    pub(crate) cuda_memory: gtk::Entry,
}

impl WorkspaceFeatureFields {
    pub(crate) fn new() -> Self {
        Self {
            docker: gtk::Switch::new(),
            graphical: gtk::Switch::new(),
            vpn: Field::entry("socks5:127.30.0.1:1080  (blank = direct)", true),
            cuda: gtk::Switch::new(),
            cuda_name: Field::entry("hl Metal (CUDA-sim) Device", true),
            cuda_capability: Field::entry("8.6", true),
            cuda_memory: Field::entry("4096", true),
        }
    }

    pub(crate) fn docker(&self) -> gtk::Box {
        Field::toggle(
            "Docker socket",
            "Make the host Docker service available inside the workspace.",
            &self.docker,
        )
    }

    pub(crate) fn applications(&self) -> gtk::Box {
        Field::toggle(
            "Graphical applications",
            "Allow browsers, editors, and other Linux apps to open windows.",
            &self.graphical,
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

    pub(crate) fn cuda(&self) -> gtk::Box {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.append(&Field::toggle(
            "CUDA device",
            "Expose a CUDA-compatible device backed by the host GPU.",
            &self.cuda,
        ));
        content.append(&Field::text("DEVICE NAME", &self.cuda_name, None));
        content.append(&Field::text(
            "COMPUTE CAPABILITY",
            &self.cuda_capability,
            None,
        ));
        content.append(&Field::text("MEMORY (MB)", &self.cuda_memory, None));
        content
    }
}

// =================================================================================================
// Window 2 — New Workspace (settings sheet)
// =================================================================================================

use crate::components::layout::Field;
/// Handles to every field, so Create can gather the full config.
use crate::*;
