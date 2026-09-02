//! Volume and network ports over the workspace's Docker-compatible daemon.

use std::sync::Arc;

use hl_extension::port::{HostError, NetworkStore, NetworkSummary, VolumeStore, VolumeSummary};

use super::{failure, Bridge};

pub struct Resources { bridge: Arc<Bridge> }

impl Resources {
    pub(super) fn new(bridge: Arc<Bridge>) -> Self { Self { bridge } }
}

fn volume(value: &hl_client::model::Volume) -> VolumeSummary {
    VolumeSummary { name: value.name.clone(), driver: value.driver.clone(), generation: value.husklet_generation.clone() }
}

fn network(value: &hl_client::model::Network) -> NetworkSummary {
    NetworkSummary { id: value.id.clone(), name: value.name.clone(), driver: value.driver.clone(), scope: value.scope.clone() }
}

impl VolumeStore for Resources {
    fn list(&self) -> Result<Vec<VolumeSummary>, HostError> {
        let listed = self.bridge.wait(self.bridge.client().volumes().list()).map_err(|error| failure(&error))?;
        Ok(listed.volumes.iter().map(volume).collect())
    }

    fn inspect(&self, name: &str) -> Result<VolumeSummary, HostError> {
        self.bridge.wait(self.bridge.client().volumes().inspect(name)).map(|value| volume(&value)).map_err(|error| failure(&error))
    }

    fn create(&self, name: &str) -> Result<VolumeSummary, HostError> {
        let request = hl_client::model::VolumeCreate { name: name.into(), ..Default::default() };
        self.bridge.wait(self.bridge.client().volumes().create(&request)).map(|value| volume(&value)).map_err(|error| failure(&error))
    }

    fn remove(&self, name: &str, generation: &str) -> Result<(), HostError> {
        self.bridge.wait(self.bridge.client().volumes().remove_if_generation(name, generation)).map_err(|error| failure(&error))
    }
}

impl NetworkStore for Resources {
    fn list(&self) -> Result<Vec<NetworkSummary>, HostError> {
        self.bridge.wait(self.bridge.client().networks().list()).map(|values| values.iter().map(network).collect()).map_err(|error| failure(&error))
    }

    fn inspect(&self, reference: &str) -> Result<NetworkSummary, HostError> {
        self.bridge.wait(self.bridge.client().networks().inspect(reference)).map(|value| network(&value)).map_err(|error| failure(&error))
    }

    fn create(&self, name: &str) -> Result<String, HostError> {
        let request = hl_client::model::NetworkCreate { name: name.into(), ..Default::default() };
        self.bridge.wait(self.bridge.client().networks().create(&request)).map(|value| value.id).map_err(|error| failure(&error))
    }

    fn remove(&self, reference: &str) -> Result<(), HostError> {
        self.bridge.wait(self.bridge.client().networks().remove(reference, false)).map_err(|error| failure(&error))
    }

    fn connect(&self, reference: &str, container: &str) -> Result<(), HostError> {
        let request = hl_client::model::NetworkConnect { container: container.into(), ..Default::default() };
        self.bridge.wait(self.bridge.client().networks().connect(reference, &request)).map_err(|error| failure(&error))
    }

    fn disconnect(&self, reference: &str, container: &str) -> Result<(), HostError> {
        let request = hl_client::model::NetworkDisconnect { container: container.into(), force: false, ..Default::default() };
        self.bridge.wait(self.bridge.client().networks().disconnect(reference, &request)).map_err(|error| failure(&error))
    }
}
