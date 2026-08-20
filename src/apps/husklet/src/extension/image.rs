//! Listing and fetching images on behalf of an extension.

use std::sync::Arc;

use hl_extension::port::{HostError, ImageStore, ImageSummary};

use super::{failure, Bridge};

/// The image port over the workspace's container daemon.
pub struct ImageLibrary {
    bridge: Arc<Bridge>,
}

impl ImageLibrary {
    pub(super) fn new(bridge: Arc<Bridge>) -> Self {
        Self { bridge }
    }
}

impl ImageStore for ImageLibrary {
    /// # Errors
    /// Returns a host failure from the container daemon.
    fn list(&self) -> Result<Vec<ImageSummary>, HostError> {
        let client = self.bridge.client();
        let images = self
            .bridge
            .wait(client.images().list())
            .map_err(|error| failure(&error))?;
        Ok(images.iter().map(summary).collect())
    }

    /// Pulls a reference and reports the image it produced.
    ///
    /// The result is read back from the local listing rather than from the
    /// progress stream, so a pull and a list describe an image identically.
    ///
    /// # Errors
    /// Returns a host failure, including a registry refusal reported inside the
    /// progress stream, and `HostError::Absent` when the pull reported success
    /// but named no local image.
    fn pull(&self, reference: &str) -> Result<ImageSummary, HostError> {
        let (name, tag) = split(reference);
        let client = self.bridge.client();
        self.bridge.wait(fetch(client, name, tag))?;
        let wanted = tagged(reference);
        self.list()?
            .into_iter()
            .find(|image| image.reference == wanted)
            .ok_or_else(|| HostError::Absent(format!("{reference} is not present after its pull")))
    }
}

/// Runs a pull to completion, surfacing the registry's own refusal.
///
/// Docker reports registry failures as records inside a successful stream, so a
/// pull that is never inspected looks like a pull that worked.
async fn fetch(client: &hl_client::Client, name: &str, tag: Option<&str>) -> Result<(), HostError> {
    let mut progress = client
        .images()
        .pull(name, tag, None)
        .await
        .map_err(|error| failure(&error))?;
    while let Some(record) = progress.next().await.map_err(|error| failure(&error))? {
        let Some(detail) = record.error else { continue };
        return Err(HostError::Failed(detail));
    }
    Ok(())
}

/// Maps a Docker image entry onto the protocol's image view.
fn summary(image: &hl_client::model::ImageSummary) -> ImageSummary {
    ImageSummary {
        id: image.id.clone(),
        reference: image.name(),
        size: u64::try_from(image.size).unwrap_or_default(),
        created: image.created,
    }
}

/// Splits a reference into the name and the tag or digest the registry wants.
///
/// The colon in a registry host's port belongs to the name, so only the part
/// after the final path separator is considered.
fn split(reference: &str) -> (&str, Option<&str>) {
    if let Some(index) = reference.find('@') {
        return (&reference[..index], Some(&reference[index + 1..]));
    }
    let start = reference.rfind('/').map_or(0, |index| index + 1);
    let Some(index) = reference[start..].rfind(':') else {
        return (reference, None);
    };
    let index = start + index;
    (&reference[..index], Some(&reference[index + 1..]))
}

/// The reference as the local listing spells it, with Docker's implied tag made
/// explicit so an untagged request still matches what was pulled.
fn tagged(reference: &str) -> String {
    match split(reference) {
        (_, Some(_)) => reference.to_owned(),
        (name, None) => format!("{name}:latest"),
    }
}

#[cfg(test)]
mod tests {
    use super::{split, summary, tagged};

    #[test]
    fn a_reference_splits_into_a_name_and_a_tag() {
        assert_eq!(split("ubuntu"), ("ubuntu", None));
        assert_eq!(split("ubuntu:24.04"), ("ubuntu", Some("24.04")));
        assert_eq!(split("library/ubuntu:24.04"), ("library/ubuntu", Some("24.04")));
    }

    #[test]
    fn a_registry_port_is_not_mistaken_for_a_tag() {
        assert_eq!(
            split("registry.example:5000/ubuntu"),
            ("registry.example:5000/ubuntu", None)
        );
        assert_eq!(
            split("registry.example:5000/ubuntu:24.04"),
            ("registry.example:5000/ubuntu", Some("24.04"))
        );
    }

    #[test]
    fn a_digest_reference_keeps_its_whole_digest() {
        assert_eq!(split("ubuntu@sha256:abc"), ("ubuntu", Some("sha256:abc")));
    }

    #[test]
    fn an_untagged_request_matches_the_tag_docker_implies() {
        assert_eq!(tagged("ubuntu"), "ubuntu:latest");
        assert_eq!(tagged("ubuntu:24.04"), "ubuntu:24.04");
    }

    #[test]
    fn an_image_entry_maps_onto_the_protocol_view() {
        let image: hl_client::model::ImageSummary = serde_json::from_value(serde_json::json!({
            "Id": "sha256:deadbeefcafe0000",
            "RepoTags": ["ubuntu:24.04"],
            "RepoDigests": [],
            "Created": 1_700_000_000_i64,
            "Size": 80_000_000_i64,
            "SharedSize": 0_i64,
            "VirtualSize": 80_000_000_i64,
            "Labels": {},
            "Containers": 0_i64
        }))
        .expect("image listing");

        let mapped = summary(&image);
        assert_eq!(mapped.id, "sha256:deadbeefcafe0000");
        assert_eq!(mapped.reference, "ubuntu:24.04");
        assert_eq!(mapped.size, 80_000_000);
        assert_eq!(mapped.created, 1_700_000_000);
    }
}
