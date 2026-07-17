//! `/networks` DTOs.

use serde::Serialize;
use std::collections::HashMap;

// ---- networks --------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct NetworkJson {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub scope: String,
    pub containers: HashMap<String, NetContainer>,
    pub created: String,
    #[serde(rename = "EnableIPv6")]
    pub enable_ipv6: bool,
    pub internal: bool,
    #[serde(rename = "IPAM")]
    pub ipam: Ipam,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct NetContainer {
    pub name: String,
    #[serde(rename = "EndpointID")]
    pub endpoint_id: String,
    pub mac_address: String,
    #[serde(rename = "IPv4Address")]
    pub ipv4_address: String,
    #[serde(rename = "IPv6Address")]
    pub ipv6_address: String,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Ipam {
    pub driver: &'static str,
    pub config: Vec<IpamConfig>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct IpamConfig {
    pub subnet: String,
    pub gateway: String,
}

/// `POST /networks/create` ack — `{"Id": <id>, "Warning": ""}`.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct NetworkCreateResponse {
    pub id: String,
    pub warning: String,
}

/// `POST /networks/prune` report — the names of the removed user-defined networks.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct NetworksPruneReport {
    pub networks_deleted: Vec<String>,
}
