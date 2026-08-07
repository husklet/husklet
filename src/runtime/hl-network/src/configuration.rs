//! Guest-visible network configuration: routes, resolvers, and search domains.
use crate::{AddressFamily, Route, SocketAddress, SocketError};
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkConfiguration {
    pub routes: Vec<Route>,
    pub dns_servers: Vec<SocketAddress>,
    pub search_domains: Vec<String>,
}

impl NetworkConfiguration {
    pub fn new(
        routes: Vec<Route>,
        dns_servers: Vec<SocketAddress>,
        search_domains: Vec<String>,
    ) -> Result<Self, SocketError> {
        if routes.len() > 256
            || dns_servers.len() > 8
            || search_domains.len() > 16
            || routes.iter().any(|route| !Self::route_valid(route))
            || dns_servers
                .iter()
                .any(|server| !matches!(server, SocketAddress::Inet4 { .. } | SocketAddress::Inet6 { .. }))
            || search_domains.iter().any(|domain| !Self::domain_valid(domain))
        {
            return Err(SocketError::Capacity);
        }
        Ok(Self {
            routes,
            dns_servers,
            search_domains,
        })
    }

    pub fn restore(snapshot: &Self) -> Result<Self, SocketError> {
        Self::new(
            snapshot.routes.clone(),
            snapshot.dns_servers.clone(),
            snapshot.search_domains.clone(),
        )
    }

    fn route_valid(route: &Route) -> bool {
        match route.family {
            AddressFamily::Inet4 => route.prefix_bits <= 32,
            AddressFamily::Inet6 => route.prefix_bits <= 128,
            AddressFamily::Unix => false,
        }
    }

    fn domain_valid(domain: &str) -> bool {
        !domain.is_empty()
            && domain.len() <= 253
            && domain.split('.').all(|label| !label.is_empty() && label.len() <= 63)
    }
}
