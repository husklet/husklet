use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

/// Transport protocol named by a container port.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Tcp,
}

/// A port exposed by the container process, whether or not it is published.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Port {
    pub guest: u16,
    pub protocol: Protocol,
}

impl Port {
    /// Creates a TCP container port.
    ///
    /// # Errors
    /// Returns an error when `guest` is zero.
    pub fn tcp(guest: u16) -> Result<Self> {
        if guest == 0 {
            return Err(Error::InvalidSpec("container port must be nonzero".into()));
        }
        Ok(Self {
            guest,
            protocol: Protocol::Tcp,
        })
    }
}

/// A host TCP port published to one container port.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Publication {
    pub host_ip: Ipv4Addr,
    pub host: u16,
    pub port: Port,
}

impl Publication {
    /// Publishes a TCP container port on a host address and port.
    ///
    /// A zero host port requests automatic allocation when the container is created.
    ///
    /// # Errors
    /// Returns an error when the container port is zero.
    pub fn tcp(host_ip: Ipv4Addr, host: u16, guest: u16) -> Result<Self> {
        Ok(Self {
            host_ip,
            host,
            port: Port::tcp(guest)?,
        })
    }

    /// Whether two publications compete for the same host TCP socket.
    #[must_use]
    pub fn conflicts(self, other: Self) -> bool {
        self.host == other.host
            && (self.host_ip.is_unspecified() || other.host_ip.is_unspecified() || self.host_ip == other.host_ip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_preserves_the_requested_host_address() {
        let publish = Publication::tcp(Ipv4Addr::LOCALHOST, 8_080, 80).unwrap();
        assert_eq!(publish.host_ip, Ipv4Addr::LOCALHOST);
        assert_eq!(publish.host, 8_080);
        assert_eq!(publish.port, Port::tcp(80).unwrap());
    }

    #[test]
    fn zero_host_port_requests_automatic_allocation() {
        let publish = Publication::tcp(Ipv4Addr::LOCALHOST, 0, 80).unwrap();
        assert_eq!(publish.host, 0);
        assert_eq!(publish.port, Port::tcp(80).unwrap());
    }

    #[test]
    fn wildcard_address_conflicts_with_every_address_on_the_same_port() {
        let wildcard = Publication::tcp(Ipv4Addr::UNSPECIFIED, 8_080, 80).unwrap();
        let loopback = Publication::tcp(Ipv4Addr::LOCALHOST, 8_080, 81).unwrap();
        let other = Publication::tcp("127.0.0.2".parse().unwrap(), 8_080, 82).unwrap();
        assert!(wildcard.conflicts(loopback));
        assert!(!loopback.conflicts(other));
    }
}
