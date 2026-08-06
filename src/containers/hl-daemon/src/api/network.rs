use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Docker request for a local network.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkCreate {
    pub name: String,
    #[serde(default)]
    pub check_duplicate: bool,
    #[serde(default)]
    pub driver: String,
    #[serde(default)]
    pub internal: bool,
    #[serde(default)]
    pub attachable: bool,
    #[serde(default)]
    pub ingress: bool,
    #[serde(default, rename = "IPAM")]
    pub ipam: Ipam,
    #[serde(default, rename = "EnableIPv6")]
    pub enable_ipv6: bool,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub config_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_from: Option<ConfigFrom>,
    /// Fields understood by newer Docker clients but not represented by this contract.
    ///
    /// The daemon validates these before creating state so meaningful options are never
    /// accepted and silently ignored.
    #[serde(flatten, default)]
    pub unsupported: BTreeMap<String, serde_json::Value>,
}

/// Network creation acknowledgement.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkCreated {
    #[serde(rename = "Id")]
    pub id: String,
    pub warning: String,
}

/// Docker network inspection response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Network {
    pub name: String,
    #[serde(rename = "Id")]
    pub id: String,
    pub created: String,
    pub scope: String,
    pub driver: String,
    #[serde(rename = "EnableIPv6")]
    pub enable_ipv6: bool,
    #[serde(rename = "IPAM")]
    pub ipam: Ipam,
    pub internal: bool,
    pub attachable: bool,
    pub ingress: bool,
    pub config_from: ConfigFrom,
    pub config_only: bool,
    pub containers: BTreeMap<String, NetworkContainer>,
    pub options: BTreeMap<String, String>,
    pub labels: BTreeMap<String, String>,
}

impl Network {
    /// Docker's conventional twelve-character display identity.
    #[must_use]
    pub fn short_id(&self) -> String {
        self.id.trim_start_matches("sha256:").chars().take(12).collect()
    }
}

/// Docker IPAM configuration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Ipam {
    #[serde(default)]
    pub driver: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub config: Vec<IpamConfig>,
    #[serde(flatten, default)]
    pub unsupported: BTreeMap<String, serde_json::Value>,
}

/// One IPAM address pool.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct IpamConfig {
    #[serde(default)]
    pub subnet: String,
    #[serde(default, rename = "IPRange")]
    pub ip_range: String,
    #[serde(default)]
    pub gateway: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auxiliary_addresses: Option<BTreeMap<String, String>>,
    #[serde(flatten, default)]
    pub unsupported: BTreeMap<String, serde_json::Value>,
}

/// Config-only network source. Unsupported nonempty values are rejected.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ConfigFrom {
    #[serde(default)]
    pub network: String,
    #[serde(flatten, default)]
    pub unsupported: BTreeMap<String, serde_json::Value>,
}

/// Container endpoint reported by network inspection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkContainer {
    pub name: String,
    #[serde(rename = "EndpointID")]
    pub endpoint_id: String,
    pub mac_address: String,
    #[serde(rename = "IPv4Address")]
    pub ipv4_address: String,
    #[serde(rename = "IPv6Address")]
    pub ipv6_address: String,
}

/// Docker connect request.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkConnect {
    pub container: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_config: Option<EndpointConfig>,
    #[serde(flatten, default)]
    pub unsupported: BTreeMap<String, serde_json::Value>,
}

/// Endpoint address and naming request.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct EndpointConfig {
    #[serde(default, rename = "IPAMConfig", skip_serializing_if = "Option::is_none")]
    pub ipam: Option<EndpointIpam>,
    #[serde(default, deserialize_with = "crate::api::null_default")]
    pub links: Vec<String>,
    #[serde(default, deserialize_with = "crate::api::null_default")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_opts: Option<BTreeMap<String, String>>,
    #[serde(flatten, default)]
    pub unsupported: BTreeMap<String, serde_json::Value>,
}

/// Static endpoint addresses requested from IPAM.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct EndpointIpam {
    #[serde(default, rename = "IPv4Address")]
    pub ipv4_address: String,
    #[serde(default, rename = "IPv6Address")]
    pub ipv6_address: String,
    #[serde(default, deserialize_with = "crate::api::null_default")]
    pub link_local_ips: Vec<String>,
    #[serde(flatten, default)]
    pub unsupported: BTreeMap<String, serde_json::Value>,
}

/// Docker disconnect request.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkDisconnect {
    pub container: String,
    #[serde(default)]
    pub force: bool,
    #[serde(flatten, default)]
    pub unsupported: BTreeMap<String, serde_json::Value>,
}

/// Network prune result.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkPrune {
    pub networks_deleted: Vec<String>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn network_short_id_delegates_to_short() {
        let network: super::Network = serde_json::from_value(serde_json::json!({
            "Name": "fixture",
            "Id": "abcdef0123456789ffff",
            "Created": "",
            "Scope": "local",
            "Driver": "bridge",
            "EnableIPv6": false,
            "IPAM": {},
            "Internal": false,
            "Attachable": true,
            "Ingress": false,
            "ConfigFrom": {},
            "ConfigOnly": false,
            "Containers": {},
            "Options": {},
            "Labels": {}
        }))
        .unwrap();
        assert_eq!(network.short_id(), "abcdef012345");
    }
}
