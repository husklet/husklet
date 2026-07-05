use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A per-container attachment to a network: the L3 identity (assigned IP) plus the name other
/// containers resolve it by. Keyed by container id in [`Net::endpoints`]. See `docs/design/netstack.md`.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Endpoint {
    pub(crate) name: String, // container name (or short id) — what `network inspect`/embedded DNS reports
    pub(crate) ip: String, // IPAM-assigned host address within the network subnet, e.g. "172.18.0.2"
}

/// A user-defined network. dd's isolation is a per-container loopback netns (see `run_in_jit`);
/// a network here is metadata plus the set of attached containers and their IPAM identities.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Net {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) driver: String,
    pub(crate) scope: String,
    #[serde(default)]
    pub(crate) containers: Vec<String>,
    #[serde(default)]
    pub(crate) created: i64,
    // IPAM (allocated at create from the 172.18.0.0/12 pool; bridge gets 172.17.0.0/16). Old state
    // files predate these — `#[serde(default)]` keeps them loadable (empty subnet => no IP reported).
    #[serde(default)]
    pub(crate) subnet: String, // e.g. "172.18.0.0/16"
    #[serde(default)]
    pub(crate) gateway: String, // e.g. "172.18.0.1" (.1 of the subnet)
    #[serde(default)]
    pub(crate) endpoints: HashMap<String, Endpoint>, // container-id -> assigned endpoint
}
