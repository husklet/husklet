use axum::http::StatusCode;
use hl_container::{EndpointSpec, NetworkDriver, NetworkSpec, Subnet};
use std::{collections::BTreeMap, net::Ipv4Addr};

use super::{ApiError, ApiResult};
use crate::api::{ConfigFrom, Ipam, IpamConfig, Network, NetworkContainer, NetworkCreate};

impl NetworkCreate {
    pub(super) fn spec(self) -> ApiResult<NetworkSpec> {
        Fields::from(&self.unsupported).reject("network create")?;
        if self.enable_ipv6
            || self.ingress
            || self.config_only
            || self
                .config_from
                .as_ref()
                .is_some_and(|value| !value.network.is_empty())
        {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "IPv6, ingress, and config-only networks are not implemented",
            ));
        }
        Fields::from(&self.ipam.unsupported).reject("network IPAM")?;
        for pool in &self.ipam.config {
            Fields::from(&pool.unsupported).reject("network IPAM pool")?;
        }
        if let Some(config) = &self.config_from {
            Fields::from(&config.unsupported).reject("network config source")?;
        }
        if !self.options.is_empty()
            || !self.scope.is_empty() && self.scope != "local"
            || self
                .ipam
                .options
                .as_ref()
                .is_some_and(|value| !value.is_empty())
            || !self.ipam.driver.is_empty() && self.ipam.driver != "default"
        {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "custom network and IPAM options are not implemented",
            ));
        }
        let driver = if self.driver.is_empty() {
            "bridge"
        } else {
            &self.driver
        };
        let mut spec = match driver {
            "none" => {
                if !self.ipam.config.is_empty() {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "none networks cannot configure IPAM",
                    ));
                }
                NetworkSpec::none(self.name)
            }
            "bridge" => {
                let pool = match self.ipam.config.as_slice() {
                    [] => {
                        let mut value = NetworkSpec::bridge_auto(self.name);
                        value.labels = self.labels;
                        return Ok(value);
                    }
                    [pool] => pool,
                    _ => {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "bridge networks require exactly one IPv4 subnet",
                        ));
                    }
                };
                if !pool.ip_range.is_empty()
                    || pool
                        .auxiliary_addresses
                        .as_ref()
                        .is_some_and(|value| !value.is_empty())
                {
                    return Err(ApiError::new(
                        StatusCode::NOT_IMPLEMENTED,
                        "IP ranges and auxiliary addresses are not implemented",
                    ));
                }
                let subnet = pool.subnet()?;
                let mut value = NetworkSpec::bridge(self.name, subnet);
                if !pool.gateway.is_empty() {
                    value = value.gateway(pool.gateway.parse().map_err(|_| {
                        ApiError::new(StatusCode::BAD_REQUEST, "invalid IPv4 gateway")
                    })?);
                }
                value
            }
            value => {
                return Err(ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    format!("network driver {value:?} is not implemented"),
                ));
            }
        };
        spec.labels = self.labels;
        Ok(spec)
    }
}

impl crate::api::EndpointConfig {
    pub(in crate::api::http) fn spec(self) -> ApiResult<EndpointSpec> {
        Fields::from(&self.unsupported).reject("network endpoint")?;
        if !self.links.is_empty()
            || self
                .driver_opts
                .as_ref()
                .is_some_and(|value| !value.is_empty())
        {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "endpoint links and driver options are not implemented",
            ));
        }
        let mut spec = EndpointSpec::default();
        for alias in self.aliases {
            spec = spec.alias(alias);
        }
        if let Some(ipam) = self.ipam {
            Fields::from(&ipam.unsupported).reject("network endpoint IPAM")?;
            if !ipam.ipv6_address.is_empty() || !ipam.link_local_ips.is_empty() {
                return Err(ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "IPv6 and link-local addresses are not implemented",
                ));
            }
            if !ipam.ipv4_address.is_empty() {
                spec = spec.address(ipam.ipv4_address.parse().map_err(|_| {
                    ApiError::new(StatusCode::BAD_REQUEST, "invalid endpoint IPv4 address")
                })?);
            }
        }
        Ok(spec)
    }
}

pub(super) struct Fields<'a>(&'a BTreeMap<String, serde_json::Value>);

impl<'a> From<&'a BTreeMap<String, serde_json::Value>> for Fields<'a> {
    fn from(fields: &'a BTreeMap<String, serde_json::Value>) -> Self {
        Self(fields)
    }
}

impl Fields<'_> {
    pub(super) fn reject(&self, context: &str) -> ApiResult<()> {
        let Some(name) = crate::api::CompatibilityFields::from(self.0).first_meaningful() else {
            return Ok(());
        };
        Err(ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            format!("{context} field {name:?} is not implemented"),
        ))
    }
}

impl From<hl_container::Network> for Network {
    fn from(value: hl_container::Network) -> Self {
        let prefix = value.subnet.map(|subnet| subnet.prefix);
        let containers = value
            .endpoints
            .into_iter()
            .map(|(id, endpoint)| {
                let mac_address = endpoint.mac_address().unwrap_or_default();
                let address = endpoint.address.map_or_else(String::new, |address| {
                    format!("{address}/{}", prefix.unwrap_or(32))
                });
                let entry = NetworkContainer {
                    name: endpoint.name,
                    endpoint_id: id.to_string(),
                    mac_address,
                    ipv4_address: address,
                    ipv6_address: String::new(),
                };
                (id.to_string(), entry)
            })
            .collect();
        let config = value
            .subnet
            .map(|subnet| IpamConfig {
                subnet: format!("{}/{}", subnet.address, subnet.prefix),
                gateway: value
                    .gateway
                    .map_or_else(String::new, |gateway| gateway.to_string()),
                ..IpamConfig::default()
            })
            .into_iter()
            .collect();
        Self {
            name: value.name,
            id: value.id.to_string(),
            created: chrono::DateTime::from_timestamp_millis(
                i64::try_from(value.created_at_ms).unwrap_or(i64::MAX),
            )
            .unwrap_or_default()
            .to_rfc3339(),
            scope: "local".into(),
            driver: match value.driver {
                NetworkDriver::None => "none",
                NetworkDriver::Bridge => "bridge",
            }
            .into(),
            enable_ipv6: false,
            ipam: Ipam {
                driver: "default".into(),
                options: None,
                config,
                ..Ipam::default()
            },
            internal: value.driver == NetworkDriver::None,
            attachable: false,
            ingress: false,
            config_from: ConfigFrom::default(),
            config_only: false,
            containers,
            options: BTreeMap::new(),
            labels: value.labels,
        }
    }
}

impl IpamConfig {
    fn subnet(&self) -> ApiResult<Subnet> {
        let (address, prefix) = self.subnet.split_once('/').ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "subnet must use IPv4 CIDR notation",
            )
        })?;
        let address: Ipv4Addr = address
            .parse()
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid IPv4 subnet"))?;
        let prefix = prefix
            .parse()
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid IPv4 prefix"))?;
        Subnet::new(address, prefix).map_err(ApiError::container)
    }
}

#[cfg(test)]
mod tests {
    use crate::api::{EndpointConfig, NetworkCreate};
    use axum::http::StatusCode;

    #[test]
    fn network_create_preserves_and_rejects_meaningful_unknown_fields() {
        let harmless: NetworkCreate = serde_json::from_value(serde_json::json!({
            "Name": "isolated",
            "Driver": "none",
            "FutureOption": false
        }))
        .unwrap();
        harmless.spec().unwrap();

        let meaningful: NetworkCreate = serde_json::from_value(serde_json::json!({
            "Name": "isolated",
            "Driver": "none",
            "FutureOption": "enabled"
        }))
        .unwrap();
        let error = meaningful.spec().unwrap_err();
        assert_eq!(error.status, StatusCode::NOT_IMPLEMENTED);
        assert!(format!("{error:?}").contains("FutureOption"));

        let nested: NetworkCreate = serde_json::from_value(serde_json::json!({
            "Name": "bridge",
            "IPAM": {"Config": [], "FuturePoolPolicy": "strict"}
        }))
        .unwrap();
        assert!(format!("{:?}", nested.spec().unwrap_err()).contains("FuturePoolPolicy"));

        let endpoint: EndpointConfig = serde_json::from_value(serde_json::json!({
            "IPAMConfig": {"IPv4Address": "10.0.0.2", "FutureRoute": true}
        }))
        .unwrap();
        assert!(format!("{:?}", endpoint.spec().unwrap_err()).contains("FutureRoute"));
    }
}
