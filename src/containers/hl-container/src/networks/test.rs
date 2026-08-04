use super::*;
use std::net::Ipv4Addr;

fn bridge(address: Ipv4Addr, prefix: u8) -> Network {
    Network::from_spec(
        NetworkSpec::bridge("test", crate::Subnet::new(address, prefix).unwrap()),
        0,
    )
}

fn occupy(network: &mut Network, address: Ipv4Addr) {
    let container = ContainerId::new();
    network.endpoints.insert(
        container.clone(),
        Endpoint {
            container,
            address: Some(address),
            name: address.to_string(),
            generated_name: false,
            aliases: Vec::new(),
        },
    );
}

#[test]
fn address_allocation_starts_at_dot2_skips_used_and_crosses_24_boundary() {
    let mut network = bridge(Ipv4Addr::new(172, 18, 0, 0), 16);
    assert_eq!(network.allocate(None).unwrap(), Some(Ipv4Addr::new(172, 18, 0, 2)));
    for fourth in 2..=255 {
        occupy(&mut network, Ipv4Addr::new(172, 18, 0, fourth));
    }
    assert_eq!(network.allocate(None).unwrap(), Some(Ipv4Addr::new(172, 18, 1, 0)));
}

#[test]
fn address_allocation_reports_true_exhaustion() {
    let mut network = bridge(Ipv4Addr::new(10, 0, 0, 0), 30);
    occupy(&mut network, Ipv4Addr::new(10, 0, 0, 2));
    assert!(matches!(network.allocate(None), Err(Error::InvalidNetwork(_))));
}
