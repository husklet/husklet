use super::ContainerId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::{fmt, str::FromStr};

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct NetworkId(String);

impl NetworkId {
    pub(crate) fn new() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NetworkId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}
impl FromStr for NetworkId {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        uuid::Uuid::parse_str(value).map_err(|_| "invalid network id")?;
        Ok(Self(value.replace('-', "")))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkDriver {
    None,
    Bridge,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Subnet {
    pub address: Ipv4Addr,
    pub prefix: u8,
}

impl Subnet {
    /// Creates a normalized IPv4 subnet.
    ///
    /// # Errors
    /// Returns an error when the prefix is invalid or the address contains host bits.
    pub fn new(address: Ipv4Addr, prefix: u8) -> crate::Result<Self> {
        if prefix > 32 {
            return Err(crate::Error::InvalidNetwork("IPv4 prefix exceeds 32".into()));
        }
        let subnet = Self { address, prefix };
        if u32::from(address) & subnet.mask() != u32::from(address) {
            return Err(crate::Error::InvalidNetwork(format!(
                "subnet address {address}/{prefix} contains host bits"
            )));
        }
        Ok(subnet)
    }

    #[must_use]
    pub fn contains(self, address: Ipv4Addr) -> bool {
        u32::from(address) & self.mask() == u32::from(self.address)
    }

    pub(crate) fn broadcast(self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.address) | !self.mask())
    }

    pub(crate) fn overlaps(self, other: Self) -> bool {
        self.contains(other.address) || other.contains(self.address)
    }

    fn mask(self) -> u32 {
        if self.prefix == 0 {
            0
        } else {
            u32::MAX << (32 - self.prefix)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkSpec {
    pub name: String,
    pub driver: NetworkDriver,
    pub subnet: Option<Subnet>,
    pub gateway: Option<Ipv4Addr>,
    pub labels: BTreeMap<String, String>,
}

impl NetworkSpec {
    #[must_use]
    pub fn none(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            driver: NetworkDriver::None,
            subnet: None,
            gateway: None,
            labels: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn bridge(name: impl Into<String>, subnet: Subnet) -> Self {
        Self {
            name: name.into(),
            driver: NetworkDriver::Bridge,
            subnet: Some(subnet),
            gateway: None,
            labels: BTreeMap::new(),
        }
    }

    /// Creates a bridge whose private IPv4 subnet is allocated atomically when
    /// the network is created.
    #[must_use]
    pub fn bridge_auto(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            driver: NetworkDriver::Bridge,
            subnet: None,
            gateway: None,
            labels: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn gateway(mut self, value: Ipv4Addr) -> Self {
        self.gateway = Some(value);
        self
    }

    #[must_use]
    pub fn label(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(name.into(), value.into());
        self
    }

    pub(crate) fn validate(&self) -> crate::Result<()> {
        let valid = !self.name.is_empty()
            && self.name.len() <= 255
            && self.name != "."
            && self.name != ".."
            && self.name.bytes().enumerate().all(|(index, byte)| match byte {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => true,
                b'_' | b'.' | b'-' => index != 0,
                _ => false,
            });
        if !valid {
            return Err(crate::Error::InvalidNetwork(format!(
                "unsafe network name {:?}",
                self.name
            )));
        }
        if self.labels.keys().any(String::is_empty) {
            return Err(crate::Error::InvalidNetwork(
                "network label names must not be empty".into(),
            ));
        }
        match (self.driver, self.subnet, self.gateway) {
            (NetworkDriver::None | NetworkDriver::Bridge, None, None) => Ok(()),
            (NetworkDriver::None, _, _) => Err(crate::Error::InvalidNetwork(
                "none networks cannot configure IPAM".into(),
            )),
            (NetworkDriver::Bridge, None, Some(_)) => Err(crate::Error::InvalidNetwork(
                "an automatic bridge subnet cannot use an explicit gateway".into(),
            )),
            (NetworkDriver::Bridge, Some(subnet), gateway) => {
                if subnet.prefix > 30 {
                    return Err(crate::Error::InvalidNetwork(
                        "bridge subnets require usable addresses".into(),
                    ));
                }
                if gateway.is_some_and(|value| {
                    !subnet.contains(value) || value == subnet.address || value == subnet.broadcast()
                }) {
                    return Err(crate::Error::InvalidNetwork(
                        "gateway must be a usable subnet address".into(),
                    ));
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EndpointSpec {
    pub address: Option<Ipv4Addr>,
    pub name: Option<String>,
    pub aliases: Vec<String>,
}

impl EndpointSpec {
    #[must_use]
    pub fn address(mut self, value: Ipv4Addr) -> Self {
        self.address = Some(value);
        self
    }
    #[must_use]
    pub fn name(mut self, value: impl Into<String>) -> Self {
        self.name = Some(value.into());
        self
    }
    #[must_use]
    pub fn alias(mut self, value: impl Into<String>) -> Self {
        self.aliases.push(value.into());
        self
    }
    pub(crate) fn validate(&self) -> crate::Result<()> {
        let mut values = self.aliases.clone();
        if let Some(name) = &self.name {
            values.push(name.clone());
        }
        if values.iter().any(|value| !valid_endpoint_name(value)) {
            return Err(crate::Error::InvalidNetwork(
                "endpoint names and aliases must be non-empty DNS-compatible names".into(),
            ));
        }
        let unique = values.iter().collect::<std::collections::BTreeSet<_>>();
        if unique.len() != values.len() {
            return Err(crate::Error::InvalidNetwork("endpoint aliases must be unique".into()));
        }
        Ok(())
    }
}

pub(crate) fn valid_endpoint_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.bytes().enumerate().all(|(index, byte)| match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => true,
            b'_' | b'.' | b'-' => index != 0,
            _ => false,
        })
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Endpoint {
    pub container: ContainerId,
    pub address: Option<Ipv4Addr>,
    pub name: String,
    /// True when `name` was copied from the container name rather than explicitly configured.
    #[serde(default)]
    pub generated_name: bool,
    pub aliases: Vec<String>,
}

impl Endpoint {
    /// Docker-compatible locally administered MAC derived from the endpoint IPv4 address.
    #[must_use]
    pub fn mac_address(&self) -> Option<String> {
        let octets = self.address?.octets();
        Some(format!(
            "02:42:{:02x}:{:02x}:{:02x}:{:02x}",
            octets[0], octets[1], octets[2], octets[3]
        ))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Network {
    pub id: NetworkId,
    pub name: String,
    pub driver: NetworkDriver,
    pub subnet: Option<Subnet>,
    pub gateway: Option<Ipv4Addr>,
    pub labels: BTreeMap<String, String>,
    pub endpoints: BTreeMap<ContainerId, Endpoint>,
    pub created_at_ms: u64,
}

impl Network {
    /// Whether this is one of the Docker-compatible networks created with the service.
    #[must_use]
    pub fn predefined(&self) -> bool {
        matches!(self.name.as_str(), "bridge" | "none")
    }

    pub(crate) fn from_spec(spec: NetworkSpec, created_at_ms: u64) -> Self {
        let gateway = spec
            .gateway
            .or_else(|| spec.subnet.map(|subnet| Ipv4Addr::from(u32::from(subnet.address) + 1)));
        Self {
            id: NetworkId::new(),
            name: spec.name,
            driver: spec.driver,
            subnet: spec.subnet,
            gateway,
            labels: spec.labels,
            endpoints: BTreeMap::new(),
            created_at_ms,
        }
    }

    pub(crate) fn compatible(&self, spec: &NetworkSpec) -> bool {
        self.driver == spec.driver
            && (spec.subnet.is_none() || self.subnet == spec.subnet)
            && self.labels == spec.labels
            && (spec.gateway.is_none() || self.gateway == spec.gateway)
    }

    pub(crate) fn allocate(&self, requested: Option<Ipv4Addr>) -> crate::Result<Option<Ipv4Addr>> {
        if self.driver == NetworkDriver::None {
            if requested.is_some() {
                return Err(crate::Error::InvalidNetwork(
                    "none networks cannot allocate addresses".into(),
                ));
            }
            return Ok(None);
        }
        let subnet = self
            .subnet
            .ok_or_else(|| crate::Error::Corrupt("bridge network omitted subnet".into()))?;
        let used = self
            .endpoints
            .values()
            .filter_map(|endpoint| endpoint.address)
            .collect::<std::collections::BTreeSet<_>>();
        if let Some(address) = requested {
            if !subnet.contains(address)
                || address == subnet.address
                || address == subnet.broadcast()
                || Some(address) == self.gateway
                || used.contains(&address)
            {
                return Err(crate::Error::InvalidNetwork(format!(
                    "address {address} is unavailable"
                )));
            }
            return Ok(Some(address));
        }
        let first = u32::from(subnet.address).saturating_add(1);
        let last = u32::from(subnet.broadcast());
        for raw in first..last {
            let address = Ipv4Addr::from(raw);
            if Some(address) != self.gateway && !used.contains(&address) {
                return Ok(Some(address));
            }
        }
        Err(crate::Error::InvalidNetwork("network address pool is exhausted".into()))
    }

    pub(crate) fn validate(&self) -> crate::Result<()> {
        uuid::Uuid::parse_str(self.id.as_str())
            .map_err(|_| crate::Error::Corrupt("network record has an invalid ID".into()))?;
        NetworkSpec {
            name: self.name.clone(),
            driver: self.driver,
            subnet: self.subnet,
            gateway: self.gateway,
            labels: self.labels.clone(),
        }
        .validate()
        .map_err(|error| crate::Error::Corrupt(error.to_string()))?;
        let mut addresses = std::collections::BTreeSet::new();
        for (container, endpoint) in &self.endpoints {
            if container != &endpoint.container {
                return Err(crate::Error::Corrupt(
                    "network endpoint key does not match its container".into(),
                ));
            }
            EndpointSpec {
                address: endpoint.address,
                name: Some(endpoint.name.clone()),
                aliases: endpoint.aliases.clone(),
            }
            .validate()
            .map_err(|error| crate::Error::Corrupt(error.to_string()))?;
            match (self.driver, self.subnet, endpoint.address) {
                (NetworkDriver::None, None, None) => {}
                (NetworkDriver::Bridge, Some(subnet), Some(address))
                    if subnet.contains(address)
                        && address != subnet.address
                        && address != subnet.broadcast()
                        && Some(address) != self.gateway
                        && addresses.insert(address) => {}
                _ => {
                    return Err(crate::Error::Corrupt(
                        "network endpoint has invalid address ownership".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_mac_address_is_deterministic_from_typed_ipv4() {
        let endpoint = Endpoint {
            container: ContainerId::new(),
            address: Some(Ipv4Addr::new(172, 18, 0, 2)),
            name: "web".into(),
            generated_name: false,
            aliases: Vec::new(),
        };
        assert_eq!(endpoint.mac_address().as_deref(), Some("02:42:ac:12:00:02"));
    }

    #[test]
    fn legacy_endpoint_names_decode_as_explicit() {
        let endpoint = Endpoint {
            container: ContainerId::new(),
            address: None,
            name: "legacy".into(),
            generated_name: true,
            aliases: Vec::new(),
        };
        let mut value = serde_json::to_value(endpoint).unwrap();
        value.as_object_mut().unwrap().remove("generated_name");
        let decoded: Endpoint = serde_json::from_value(value).unwrap();
        assert!(!decoded.generated_name);
    }
}
