//! A reference extension, and the shape every extension follows.
//!
//! It holds no toolkit and opens nothing itself. It observes containers through
//! the protocol, describes an interface for them, and answers the row windows
//! the host asks for — which is the whole contract an extension has.

mod catalogue;
mod session;
mod view;

pub use catalogue::Catalogue;
pub use session::{serve, Outcome};
pub use view::{Actions, View};

use hl_gui::RowRequest;
use hl_ws_extension::port::ContainerSummary;
use hl_ws_extension::{Capability, Grant, Manifest, RelativePath, Request, Topic};

/// The source the container table is drawn from.
pub const SOURCE: hl_gui::SourceId = hl_gui::SourceId::new(1);

/// What this extension asks to be allowed to do.
///
/// Read-only: it lists and follows containers, and draws. It deliberately does
/// not request control, because nothing it does needs to change a container.
#[must_use]
pub fn requested() -> Grant {
    Grant::new([Capability::ContainerRead, Capability::Interface])
}

/// The manifest this extension ships in its image label.
///
/// # Errors
/// Returns why the manifest could not be built.
pub fn manifest() -> Result<Manifest, hl_ws_extension::Invalid> {
    Ok(Manifest {
        name: hl_ws_extension::ExtensionName::new("containers")?,
        display_name: "Containers".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol: hl_ws_extension::PROTOCOL,
        capabilities: requested(),
        entrypoint: Some(vec!["/usr/local/bin/hl-extension-containers".into()]),
        activation: hl_ws_extension::Activation::Tab,
        interface: Some(hl_ws_extension::Presentation {
            tab_title: "Containers".into(),
            icon: Some("view-list-symbolic".into()),
        }),
        resources: hl_ws_extension::Resources::default(),
        filesystem_roots: Vec::<RelativePath>::new(),
    })
}

/// The calls this extension makes when it starts.
///
/// Order matters: a tab has to exist before an interface can be drawn into it.
#[must_use]
pub fn opening() -> Vec<Request> {
    vec![
        Request::InterfaceOpenTab {
            title: "Containers".into(),
        },
        Request::EventSubscribe {
            topic: Topic::Containers,
        },
        Request::ContainerList,
    ]
}

/// One extension, holding what it knows and what it has drawn.
pub struct Extension {
    catalogue: Catalogue,
    view: View,
}

impl Default for Extension {
    fn default() -> Self {
        Self::new()
    }
}

impl Extension {
    #[must_use]
    pub fn new() -> Self {
        Self {
            catalogue: Catalogue::new(),
            view: View::new(),
        }
    }

    /// Containers currently known.
    #[must_use]
    pub fn containers(&self) -> &[ContainerSummary] {
        self.catalogue.containers()
    }

    /// Records a new listing and returns the calls it produces: the interface
    /// to draw, and the new row count for the table.
    pub fn observe(&mut self, containers: Vec<ContainerSummary>) -> Vec<Request> {
        self.catalogue.replace(containers);
        let mut calls = vec![Request::InterfaceRender {
            frame: self.view.render(&self.catalogue),
        }];
        calls.extend(self.catalogue.resize());
        calls
    }

    /// Answers one window of rows.
    #[must_use]
    pub fn answer(&self, request: &RowRequest) -> hl_gui::RowWindow {
        self.catalogue.window(request)
    }
}
