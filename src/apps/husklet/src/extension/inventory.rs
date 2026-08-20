//! Reading container state on behalf of an extension.

use std::sync::Arc;

use hl_client::model::{Container, InspectContainer, List};
use hl_extension::port::{ContainerInventory, ContainerSummary, HostError};

use super::{failure, Bridge};

/// The container reading port over the workspace's container daemon.
pub struct ContainerCatalog {
    bridge: Arc<Bridge>,
}

impl ContainerCatalog {
    pub(super) fn new(bridge: Arc<Bridge>) -> Self {
        Self { bridge }
    }
}

impl ContainerInventory for ContainerCatalog {
    /// Lists every container, stopped ones included: an extension that can only
    /// see running containers cannot tell "gone" from "not started".
    ///
    /// # Errors
    /// Returns a host failure from the container daemon.
    fn list(&self) -> Result<Vec<ContainerSummary>, HostError> {
        let client = self.bridge.client();
        let containers = self
            .bridge
            .wait(client.containers().list(List::default().all()))
            .map_err(|error| failure(&error))?;
        Ok(containers.iter().map(summary).collect())
    }

    /// # Errors
    /// Returns `HostError::Absent` when no such container exists.
    fn inspect(&self, id: &str) -> Result<ContainerSummary, HostError> {
        let client = self.bridge.client();
        let container = self
            .bridge
            .wait(client.containers().inspect(id))
            .map_err(|error| failure(&error))?;
        Ok(inspection(&container))
    }
}

/// Maps a Docker list entry onto the protocol's container view.
fn summary(container: &Container) -> ContainerSummary {
    ContainerSummary {
        id: container.details.metadata.id.clone(),
        name: container.names.first().map_or_else(
            || container.details.metadata.id.clone(),
            |name| name.trim_start_matches('/').to_owned(),
        ),
        image: container.details.metadata.image.clone(),
        state: container.state.clone(),
        created: container.created,
    }
}

/// Maps a Docker inspection onto the same view.
///
/// Inspection reports creation as an RFC 3339 instant while the list reports
/// epoch seconds, so the timestamp is converted here rather than leaking two
/// spellings of one field to extensions.
fn inspection(container: &InspectContainer) -> ContainerSummary {
    ContainerSummary {
        id: container.details.metadata.id.clone(),
        name: container.name.trim_start_matches('/').to_owned(),
        image: container.details.metadata.image.clone(),
        state: container.state.status.clone(),
        created: epoch_seconds(&container.created).unwrap_or_default(),
    }
}

/// Converts `YYYY-MM-DDThh:mm:ss[.fraction][Z]` to whole seconds since the epoch.
///
/// Returns `None` for anything it cannot read, because a creation instant is
/// descriptive and a container that exists must still be reportable.
fn epoch_seconds(timestamp: &str) -> Option<i64> {
    let (date, clock) = timestamp.split_once('T')?;
    let clock = clock.trim_end_matches('Z');
    let clock = clock.split_once('.').map_or(clock, |(whole, _)| whole);
    let mut fields = date.splitn(3, '-');
    let year: i64 = fields.next()?.parse().ok()?;
    let month: i64 = fields.next()?.parse().ok()?;
    let day: i64 = fields.next()?.parse().ok()?;
    let mut parts = clock.splitn(3, ':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let second: i64 = parts.next()?.parse().ok()?;
    Some(civil_days(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days from the epoch to a proleptic Gregorian date, by the standard
/// era-based formulation, so no calendar table or clock crate is needed.
fn civil_days(year: i64, month: i64, day: i64) -> i64 {
    let shifted = year - i64::from(month <= 2);
    let era = shifted.div_euclid(400);
    let year_of_era = shifted - era * 400;
    let month_prime = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::{civil_days, epoch_seconds, inspection, summary};
    use hl_client::model::{Container, InspectContainer};

    fn listing() -> Container {
        serde_json::from_value(serde_json::json!({
            "Id": "c0ffee",
            "Names": ["/demo"],
            "Image": "ubuntu:24.04",
            "Command": "sleep",
            "Created": 1_700_000_000_i64,
            "State": "running",
            "Status": "Up 3 minutes",
            "Ports": [],
            "Mounts": [],
            "Labels": {}
        }))
        .expect("container listing")
    }

    #[test]
    fn a_listing_maps_onto_the_protocol_view() {
        let mapped = summary(&listing());
        assert_eq!(mapped.id, "c0ffee");
        assert_eq!(mapped.name, "demo", "the Docker leading slash is not an identity");
        assert_eq!(mapped.image, "ubuntu:24.04");
        assert_eq!(mapped.state, "running");
        assert_eq!(mapped.created, 1_700_000_000);
    }

    #[test]
    fn a_listing_without_a_name_falls_back_to_its_identity() {
        let mut container = listing();
        container.names.clear();
        assert_eq!(summary(&container).name, "c0ffee");
    }

    #[test]
    fn an_inspection_maps_its_state_and_instant() {
        let container: InspectContainer = serde_json::from_value(serde_json::json!({
            "Id": "c0ffee",
            "Image": "ubuntu:24.04",
            "Mounts": [],
            "Path": "/bin/sh",
            "Args": [],
            "Name": "/demo",
            "Created": "2023-11-14T22:13:20.000000000Z",
            "State": {
                "Status": "exited",
                "Running": false,
                "Paused": false,
                "Restarting": false,
                "OOMKilled": false,
                "Dead": false,
                "Pid": 0,
                "ExitCode": 0,
                "Error": "",
                "StartedAt": "",
                "FinishedAt": ""
            },
            "RestartCount": 0,
            "Config": { "ExposedPorts": {}, "Labels": {}, "StopSignal": "SIGTERM", "StopTimeout": 10 },
            "HostConfig": {
                "NetworkMode": "bridge",
                "AutoRemove": false,
                "RestartPolicy": { "Name": "no", "MaximumRetryCount": 0 }
            },
            "NetworkSettings": { "Ports": {}, "Networks": {} }
        }))
        .expect("container inspection");

        let mapped = inspection(&container);
        assert_eq!(mapped.name, "demo");
        assert_eq!(mapped.state, "exited");
        assert_eq!(mapped.created, 1_700_000_000, "the same instant the listing reports");
    }

    #[test]
    fn known_instants_convert() {
        assert_eq!(civil_days(1970, 1, 1), 0);
        assert_eq!(epoch_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_seconds("2000-03-01T00:00:01Z"), Some(951_868_801));
        assert_eq!(epoch_seconds("1969-12-31T23:59:59Z"), Some(-1));
    }

    #[test]
    fn an_unreadable_instant_does_not_hide_the_container() {
        assert_eq!(epoch_seconds("not a timestamp"), None);
        assert_eq!(epoch_seconds("2023-11-14"), None);
    }
}
