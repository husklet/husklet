//! Route entries and the longest-prefix table that selects between them.
use crate::{AddressFamily, NetworkConfiguration, SocketError};
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Route {
    pub family: AddressFamily,
    pub destination: [u8; 16],
    pub prefix_bits: u8,
    pub gateway: Option<[u8; 16]>,
    pub interface: u32,
    pub metric: u32,
}

pub struct RouteTable {
    routes: Vec<Route>,
}

impl RouteTable {
    pub fn new(routes: Vec<Route>) -> Result<Self, SocketError> {
        NetworkConfiguration::new(routes.clone(), Vec::new(), Vec::new())?;
        Ok(Self { routes })
    }

    #[must_use]
    pub fn lookup(&self, family: AddressFamily, address: [u8; 16]) -> Option<&Route> {
        self.routes
            .iter()
            .enumerate()
            .filter(|(_, route)| route.family == family && Self::matches(route, address))
            .max_by(|(left_index, left), (right_index, right)| {
                left.prefix_bits
                    .cmp(&right.prefix_bits)
                    .then_with(|| right.metric.cmp(&left.metric))
                    .then_with(|| right_index.cmp(left_index))
            })
            .map(|(_, route)| route)
    }

    fn matches(route: &Route, address: [u8; 16]) -> bool {
        let full = usize::from(route.prefix_bits / 8);
        let remainder = route.prefix_bits % 8;
        if address[..full] != route.destination[..full] {
            return false;
        }
        if remainder == 0 {
            return true;
        }
        let mask = u8::MAX << (8 - remainder);
        address[full] & mask == route.destination[full] & mask
    }
}
