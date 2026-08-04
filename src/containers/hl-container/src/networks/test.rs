use super::*;
use crate::storage::{Containers, Memory, NetworkStore};
use async_trait::async_trait;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicUsize, Ordering};

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

struct CountedNetworks {
    inner: Arc<Memory>,
    lists: AtomicUsize,
}

impl CountedNetworks {
    fn new(inner: Arc<Memory>) -> Self {
        Self {
            inner,
            lists: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl NetworkStore for CountedNetworks {
    async fn list(&self) -> Result<Vec<Network>> {
        self.lists.fetch_add(1, Ordering::Relaxed);
        NetworkStore::list(self.inner.as_ref()).await
    }

    async fn get(&self, name: &str) -> Result<Option<Network>> {
        NetworkStore::get(self.inner.as_ref(), name).await
    }

    async fn insert(&self, network: &Network) -> Result<()> {
        NetworkStore::insert(self.inner.as_ref(), network).await
    }

    async fn replace(&self, network: &Network) -> Result<()> {
        NetworkStore::replace(self.inner.as_ref(), network).await
    }

    async fn remove(&self, name: &str) -> Result<()> {
        NetworkStore::remove(self.inner.as_ref(), name).await
    }
}

fn container(name: &str, state: crate::ContainerState) -> Container {
    Container {
        id: ContainerId::new(),
        spec: crate::ContainerSpec::from_directory("/rootfs", crate::Process::new("/bin/true")).name(name),
        state,
        created_at_ms: 0,
        generation: 0,
        restart: crate::Restart::default(),
        health: None,
        checkpoint: None,
    }
}

async fn connect_list_count(peers: u8) -> usize {
    let records = Arc::new(Memory::default());
    let networks = Arc::new(CountedNetworks::new(Arc::clone(&records)));
    let root = tempfile::tempdir().unwrap();
    let service = Networks::new(
        networks.clone(),
        records.clone(),
        Arc::new(Mutex::new(())),
        root.path().to_owned(),
    );
    let target = container("target", crate::ContainerState::Created);
    Containers::insert(records.as_ref(), &target).await.unwrap();

    let mut network = bridge(Ipv4Addr::new(172, 29, 0, 0), 16);
    for index in 0..peers {
        let member = container(
            &format!("member-{index}"),
            crate::ContainerState::Running {
                process_id: u64::from(index) + 1,
                started_at_ms: 1,
            },
        );
        network.endpoints.insert(
            member.id.clone(),
            Endpoint {
                container: member.id.clone(),
                address: Some(Ipv4Addr::new(172, 29, 0, index + 2)),
                name: member.spec.name.clone().unwrap(),
                generated_name: true,
                aliases: Vec::new(),
            },
        );
        Containers::insert(records.as_ref(), &member).await.unwrap();
    }
    NetworkStore::insert(networks.as_ref(), &network).await.unwrap();

    service
        .connect(&network.name, target.id.as_str(), EndpointSpec::default())
        .await
        .unwrap();

    networks.lists.load(Ordering::Relaxed)
}

#[tokio::test]
async fn connect_inventory_bound() {
    assert_eq!(connect_list_count(0).await, 1);
    assert_eq!(connect_list_count(32).await, 2);
}
