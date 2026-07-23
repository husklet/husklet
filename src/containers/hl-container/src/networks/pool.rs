use crate::{Error, Network, Result, Subnet};
use std::net::Ipv4Addr;

pub(super) struct Pool<'a>(&'a [Network]);

impl<'a> From<&'a [Network]> for Pool<'a> {
    fn from(networks: &'a [Network]) -> Self {
        Self(networks)
    }
}

impl Pool<'_> {
    pub(super) fn allocate(&self) -> Result<Subnet> {
        let occupied = self.0.iter().filter_map(|network| network.subnet);
        for second in 18..=31 {
            let candidate = Subnet::new(Ipv4Addr::new(172, second, 0, 0), 16)?;
            if occupied.clone().all(|subnet| !subnet.overlaps(candidate)) {
                return Ok(candidate);
            }
        }
        Err(Error::InvalidNetwork(
            "automatic bridge address pools are exhausted".into(),
        ))
    }

    pub(super) fn validate(&self, candidate: Subnet) -> Result<()> {
        if self
            .0
            .iter()
            .filter_map(|network| network.subnet)
            .any(|subnet| subnet.overlaps(candidate))
        {
            return Err(Error::InvalidNetwork(format!(
                "subnet {}/{} overlaps an existing network",
                candidate.address, candidate.prefix
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Pool;
    use crate::{Error, Network, NetworkSpec, Subnet};
    use std::net::Ipv4Addr;

    fn bridge(address: Ipv4Addr, prefix: u8) -> Network {
        Network::from_spec(
            NetworkSpec::bridge("test", Subnet::new(address, prefix).unwrap()),
            0,
        )
    }

    #[test]
    fn starts_at_172_18_and_skips_occupied_ranges() {
        assert_eq!(
            Pool::from([].as_slice()).allocate().unwrap(),
            Subnet::new(Ipv4Addr::new(172, 18, 0, 0), 16).unwrap()
        );
        let occupied = [18, 19]
            .into_iter()
            .map(|second| bridge(Ipv4Addr::new(172, second, 0, 0), 16))
            .collect::<Vec<_>>();
        assert_eq!(
            Pool::from(occupied.as_slice()).allocate().unwrap(),
            Subnet::new(Ipv4Addr::new(172, 20, 0, 0), 16).unwrap()
        );
    }

    #[test]
    fn reports_pool_exhaustion_without_collision() {
        let occupied = (18..=31)
            .map(|second| bridge(Ipv4Addr::new(172, second, 0, 0), 16))
            .collect::<Vec<_>>();
        assert!(matches!(
            Pool::from(occupied.as_slice()).allocate(),
            Err(Error::InvalidNetwork(_))
        ));
    }

    #[test]
    fn rejects_an_explicit_overlapping_subnet() {
        let occupied = [bridge(Ipv4Addr::new(172, 18, 0, 0), 16)];
        assert!(matches!(
            Pool::from(occupied.as_slice())
                .validate(Subnet::new(Ipv4Addr::new(172, 18, 2, 0), 24).unwrap()),
            Err(Error::InvalidNetwork(_))
        ));
    }
}
