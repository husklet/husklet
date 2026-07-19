//! View models for the hl daemon, built from [`bollard`]'s Docker-API responses. These are the
//! shapes the hl GUI and CLI render; each has a `From<bollard::...>` conversion and the small
//! display helpers (`short_id`, `name`, `ports_str`, …) the UI relies on.

mod container;
mod image;
mod network;
mod system;
mod volume;

pub use container::*;
pub use image::*;
pub use network::*;
pub use system::*;
pub use volume::*;

/// Ordered user and driver metadata shared by Docker resources.
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub labels: Vec<(String, String)>,
    pub options: Vec<(String, String)>,
}

impl Metadata {
    pub fn new(
        labels: std::collections::HashMap<String, String>,
        options: std::collections::HashMap<String, String>,
    ) -> Self {
        Self {
            labels: labels
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_iter()
                .collect(),
            options: options
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        }
    }
}
