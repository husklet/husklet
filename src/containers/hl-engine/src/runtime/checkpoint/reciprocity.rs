//! Reciprocal socket topology over the sealed capture inventories.
//!
//! This is the capture-side mirror of `ckpt_prepare_restore_sockets`
//! (`linux_abi/checkpoint/socket_restore.c:1-95`): restore reads every
//! `proc.<gpid>/fds` inventory, keys the `CKF_SOCKETPAIR` records by their
//! object identity, and refuses a pair whose two endpoints disagree. The same
//! records reach the broker as image bytes, so the broker can discharge the
//! obligation *before* a generation exists rather than discovering it on the
//! way back in.
//!
//! Why the broker and not the coordinator: `SOURCE_LIST`/`SOURCE_SIZE`/
//! `SOURCE_READ` resolve `Server::source`, the previously published image a
//! restore reads, and `CheckpointSink` has no read method at all, so a
//! capture-side read-back would answer from the wrong generation. The broker
//! already holds every committed group's bytes on their way to the sink and
//! already refuses a publisher that has not passed `REGISTER_READY`
//! (`broker.rs::publishes_capture_bytes`). Owner-membership therefore comes
//! free: an endpoint that appears in a committed inventory is by construction
//! owned by a sealed member of this capture.
//!
//! What this proves: over the whole sealed tree, every captured `AF_UNIX` pair
//! endpoint is named exactly once, by exactly one owner, and names back exactly
//! one endpoint that names it in return with the same socket type.
//!
//! What it does not prove: that the far end was quiescent for any reason other
//! than membership in the freeze, that unlinked host peers outside the guest
//! tree exist, or anything about queue contents. It is a topology proof, not a
//! liveness one.

use std::collections::BTreeMap;

/// `CKF_SOCKETPAIR` in `linux_abi/checkpoint/capture.c:91`.
const SOCKETPAIR_KIND: i32 = 10;

/// `sizeof(struct ckpt_fd)` — `capture.c:172-179`: four `int32_t`, an `int64_t`,
/// three `uint64_t`, then a 512-byte path.
const RECORD_BYTES: usize = 560;
const OFFSET_KIND: usize = 4;
const OFFSET_TYPE: usize = 16;
const OFFSET_OBJECT: usize = 24;
const OFFSET_PEER: usize = 40;

/// `SOCK_STREAM`, `SOCK_DGRAM`, `SOCK_SEQPACKET` on Linux, the three types
/// `ckpt_prepare_restore_sockets` will rebuild.
const SOCKET_TYPES: [i64; 3] = [1, 2, 5];

/// One captured endpoint, named by the group that published it so every refusal
/// can point at a process rather than at the tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Endpoint {
    group: u64,
    guest_descriptor: i32,
    peer: u64,
    socket_type: i64,
}

/// A named, specific reason one capture's socket topology is not reciprocal.
///
/// Every variant carries the object, the owning group and the invariant, because
/// an anonymous refusal here costs a full instrumented acceptance run to
/// diagnose.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Violation {
    /// A `proc.<gpid>/fds` object is not a whole number of `struct ckpt_fd`.
    TruncatedInventory { group: u64, bytes: usize },
    /// An endpoint carries no object identity, or names no peer.
    UnidentifiedEndpoint {
        group: u64,
        guest_descriptor: i32,
        object: u64,
        peer: u64,
    },
    /// An endpoint names itself as its own reciprocal peer.
    SelfReciprocal { group: u64, object: u64 },
    /// The record's socket type is not one restore can rebuild.
    UnsupportedSocketType { group: u64, object: u64, socket_type: i64 },
    /// The same object identity is owned by two descriptors. Exactly one owner
    /// is the whole point: two owners means the identity does not name an
    /// endpoint.
    DuplicateOwner {
        object: u64,
        first_group: u64,
        first_descriptor: i32,
        second_group: u64,
        second_descriptor: i32,
    },
    /// The named peer is owned by nobody in the sealed capture, so the far end
    /// of this connection was never frozen and its queue was never captured.
    MissingReciprocalPeer {
        group: u64,
        guest_descriptor: i32,
        object: u64,
        peer: u64,
    },
    /// The named peer exists but names a third object back.
    AsymmetricReciprocal {
        object: u64,
        group: u64,
        peer: u64,
        peer_group: u64,
        peer_names: u64,
    },
    /// The two ends disagree about the socket type; restore refuses this pair.
    ReciprocalTypeMismatch {
        object: u64,
        socket_type: i64,
        peer: u64,
        peer_socket_type: i64,
    },
}

impl std::fmt::Display for Violation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::TruncatedInventory { group, bytes } => write!(
                formatter,
                "proc.{group}/fds is {bytes} bytes, not a whole number of {RECORD_BYTES}-byte descriptor records"
            ),
            Self::UnidentifiedEndpoint {
                group,
                guest_descriptor,
                object,
                peer,
            } => write!(
                formatter,
                "socket fd {guest_descriptor} in proc.{group} carries object={object:016x} peer={peer:016x}; \
                 an endpoint with no identity cannot be joined"
            ),
            Self::SelfReciprocal { group, object } => write!(
                formatter,
                "socket object {object:016x} in proc.{group} names itself as its reciprocal peer"
            ),
            Self::UnsupportedSocketType {
                group,
                object,
                socket_type,
            } => write!(
                formatter,
                "socket object {object:016x} in proc.{group} has type {socket_type}, which restore cannot rebuild"
            ),
            Self::DuplicateOwner {
                object,
                first_group,
                first_descriptor,
                second_group,
                second_descriptor,
            } => write!(
                formatter,
                "socket object {object:016x} is owned twice: proc.{first_group} fd {first_descriptor} and \
                 proc.{second_group} fd {second_descriptor}"
            ),
            Self::MissingReciprocalPeer {
                group,
                guest_descriptor,
                object,
                peer,
            } => write!(
                formatter,
                "socket object {object:016x} (proc.{group} fd {guest_descriptor}) names peer {peer:016x}, \
                 which no process in this capture owns"
            ),
            Self::AsymmetricReciprocal {
                object,
                group,
                peer,
                peer_group,
                peer_names,
            } => write!(
                formatter,
                "socket object {object:016x} (proc.{group}) names peer {peer:016x}, but proc.{peer_group} names \
                 {peer_names:016x} back"
            ),
            Self::ReciprocalTypeMismatch {
                object,
                socket_type,
                peer,
                peer_socket_type,
            } => write!(
                formatter,
                "socket object {object:016x} has type {socket_type} and its peer {peer:016x} has type \
                 {peer_socket_type}"
            ),
        }
    }
}

/// Retains the `proc.<gpid>/fds` inventories of a capture as they are published,
/// and joins them once every group has committed.
#[derive(Default)]
pub(super) struct SocketTopology {
    inventories: BTreeMap<u64, Vec<u8>>,
}

impl SocketTopology {
    /// Records an object if it is a descriptor inventory. Any other name is not
    /// part of the join and is ignored.
    pub(super) fn observe(&mut self, name: &str, bytes: &[u8]) {
        let Some(group) = inventory_group(name) else {
            return;
        };
        self.inventories.insert(group, bytes.to_vec());
    }

    /// Proves reciprocity over every inventory published so far.
    ///
    /// Owner-membership is not re-checked here and does not need to be: these
    /// bytes reached the broker through `publishes_capture_bytes`, which refuses
    /// any process that has not registered as an exact member.
    pub(super) fn join(&self) -> Result<usize, Violation> {
        let mut owners: BTreeMap<u64, Endpoint> = BTreeMap::new();
        for (group, bytes) in &self.inventories {
            let group = *group;
            if bytes.len() % RECORD_BYTES != 0 {
                return Err(Violation::TruncatedInventory {
                    group,
                    bytes: bytes.len(),
                });
            }
            for record in bytes.chunks_exact(RECORD_BYTES) {
                if read_i32(record, OFFSET_KIND) != SOCKETPAIR_KIND {
                    continue;
                }
                let guest_descriptor = read_i32(record, 0);
                let object = read_u64(record, OFFSET_OBJECT);
                let peer = read_u64(record, OFFSET_PEER);
                let socket_type = read_i64(record, OFFSET_TYPE);
                if object == 0 || peer == 0 {
                    return Err(Violation::UnidentifiedEndpoint {
                        group,
                        guest_descriptor,
                        object,
                        peer,
                    });
                }
                if object == peer {
                    return Err(Violation::SelfReciprocal { group, object });
                }
                if !SOCKET_TYPES.contains(&socket_type) {
                    return Err(Violation::UnsupportedSocketType {
                        group,
                        object,
                        socket_type,
                    });
                }
                let endpoint = Endpoint {
                    group,
                    guest_descriptor,
                    peer,
                    socket_type,
                };
                // The same object reaching a second descriptor in the same
                // process is the same defect as reaching a second process: the
                // identity has stopped naming one endpoint either way.
                if let Some(first) = owners.insert(object, endpoint)
                    && (first.group != group || first.guest_descriptor != guest_descriptor)
                {
                    return Err(Violation::DuplicateOwner {
                        object,
                        first_group: first.group,
                        first_descriptor: first.guest_descriptor,
                        second_group: group,
                        second_descriptor: guest_descriptor,
                    });
                }
            }
        }
        for (object, endpoint) in &owners {
            let object = *object;
            let Some(peer) = owners.get(&endpoint.peer) else {
                return Err(Violation::MissingReciprocalPeer {
                    group: endpoint.group,
                    guest_descriptor: endpoint.guest_descriptor,
                    object,
                    peer: endpoint.peer,
                });
            };
            if peer.peer != object {
                return Err(Violation::AsymmetricReciprocal {
                    object,
                    group: endpoint.group,
                    peer: endpoint.peer,
                    peer_group: peer.group,
                    peer_names: peer.peer,
                });
            }
            if peer.socket_type != endpoint.socket_type {
                return Err(Violation::ReciprocalTypeMismatch {
                    object,
                    socket_type: endpoint.socket_type,
                    peer: endpoint.peer,
                    peer_socket_type: peer.socket_type,
                });
            }
        }
        Ok(owners.len())
    }
}

/// `proc.<gpid>/fds`, the one object name this join reads.
fn inventory_group(name: &str) -> Option<u64> {
    let (group, object) = name.split_once('/')?;
    if object != "fds" {
        return None;
    }
    group.strip_prefix("proc.")?.parse().ok()
}

fn read_i32(record: &[u8], offset: usize) -> i32 {
    i32::from_ne_bytes(record[offset..offset + 4].try_into().expect("record bounds"))
}

fn read_i64(record: &[u8], offset: usize) -> i64 {
    i64::from_ne_bytes(record[offset..offset + 8].try_into().expect("record bounds"))
}

fn read_u64(record: &[u8], offset: usize) -> u64 {
    u64::from_ne_bytes(record[offset..offset + 8].try_into().expect("record bounds"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(guest_descriptor: i32, kind: i32, socket_type: i64, object: u64, peer: u64) -> Vec<u8> {
        let mut bytes = vec![0_u8; RECORD_BYTES];
        bytes[0..4].copy_from_slice(&guest_descriptor.to_ne_bytes());
        bytes[OFFSET_KIND..OFFSET_KIND + 4].copy_from_slice(&kind.to_ne_bytes());
        bytes[OFFSET_TYPE..OFFSET_TYPE + 8].copy_from_slice(&socket_type.to_ne_bytes());
        bytes[OFFSET_OBJECT..OFFSET_OBJECT + 8].copy_from_slice(&object.to_ne_bytes());
        bytes[OFFSET_PEER..OFFSET_PEER + 8].copy_from_slice(&peer.to_ne_bytes());
        bytes
    }

    fn socket(guest_descriptor: i32, object: u64, peer: u64) -> Vec<u8> {
        record(guest_descriptor, SOCKETPAIR_KIND, 1, object, peer)
    }

    fn topology(groups: &[(&str, Vec<u8>)]) -> SocketTopology {
        let mut topology = SocketTopology::default();
        for (name, bytes) in groups {
            topology.observe(name, bytes);
        }
        topology
    }

    #[test]
    fn a_reciprocal_pair_across_two_members_is_admitted() {
        let topology = topology(&[
            ("proc.1/fds", socket(10, 0x003c_a832_0000_0002, 0x003c_ac81_0000_0001)),
            ("proc.2/fds", socket(7, 0x003c_ac81_0000_0001, 0x003c_a832_0000_0002)),
        ]);
        assert_eq!(topology.join(), Ok(2));
    }

    #[test]
    fn an_endpoint_whose_peer_no_member_owns_is_refused_by_name() {
        let topology = topology(&[("proc.1/fds", socket(10, 0x11, 0x22))]);
        assert_eq!(
            topology.join(),
            Err(Violation::MissingReciprocalPeer {
                group: 1,
                guest_descriptor: 10,
                object: 0x11,
                peer: 0x22,
            })
        );
        assert!(topology.join().unwrap_err().to_string().contains("0000000000000011"));
    }

    #[test]
    fn one_object_owned_by_two_members_is_refused_by_name() {
        let mut first = socket(10, 0x11, 0x22);
        first.extend(socket(11, 0x22, 0x11));
        let topology = topology(&[("proc.1/fds", first), ("proc.2/fds", socket(4, 0x11, 0x22))]);
        assert_eq!(
            topology.join(),
            Err(Violation::DuplicateOwner {
                object: 0x11,
                first_group: 1,
                first_descriptor: 10,
                second_group: 2,
                second_descriptor: 4,
            })
        );
    }

    #[test]
    fn two_descriptors_in_one_process_sharing_an_object_are_refused() {
        let mut inventory = socket(10, 0x11, 0x22);
        inventory.extend(socket(12, 0x11, 0x22));
        let topology = topology(&[("proc.1/fds", inventory)]);
        assert!(matches!(topology.join(), Err(Violation::DuplicateOwner { .. })));
    }

    #[test]
    fn a_peer_that_names_a_third_object_back_is_refused() {
        let topology = topology(&[
            ("proc.1/fds", socket(10, 0x11, 0x22)),
            ("proc.2/fds", socket(7, 0x22, 0x33)),
            ("proc.3/fds", socket(5, 0x33, 0x22)),
        ]);
        assert_eq!(
            topology.join(),
            Err(Violation::AsymmetricReciprocal {
                object: 0x11,
                group: 1,
                peer: 0x22,
                peer_group: 2,
                peer_names: 0x33,
            })
        );
    }

    #[test]
    fn the_two_ends_must_agree_on_the_socket_type() {
        let topology = topology(&[
            ("proc.1/fds", record(10, SOCKETPAIR_KIND, 1, 0x11, 0x22)),
            ("proc.2/fds", record(7, SOCKETPAIR_KIND, 2, 0x22, 0x11)),
        ]);
        assert_eq!(
            topology.join(),
            Err(Violation::ReciprocalTypeMismatch {
                object: 0x11,
                socket_type: 1,
                peer: 0x22,
                peer_socket_type: 2,
            })
        );
    }

    #[test]
    fn an_endpoint_without_an_identity_is_refused() {
        let topology = topology(&[("proc.4/fds", socket(9, 0x11, 0))]);
        assert_eq!(
            topology.join(),
            Err(Violation::UnidentifiedEndpoint {
                group: 4,
                guest_descriptor: 9,
                object: 0x11,
                peer: 0,
            })
        );
    }

    #[test]
    fn a_self_reciprocal_endpoint_is_refused() {
        let topology = topology(&[("proc.4/fds", socket(9, 0x11, 0x11))]);
        assert_eq!(
            topology.join(),
            Err(Violation::SelfReciprocal { group: 4, object: 0x11 })
        );
    }

    #[test]
    fn a_socket_type_restore_cannot_rebuild_is_refused() {
        let topology = topology(&[("proc.4/fds", record(9, SOCKETPAIR_KIND, 3, 0x11, 0x22))]);
        assert_eq!(
            topology.join(),
            Err(Violation::UnsupportedSocketType {
                group: 4,
                object: 0x11,
                socket_type: 3,
            })
        );
    }

    #[test]
    fn a_truncated_inventory_is_refused_rather_than_read_short() {
        let mut bytes = socket(9, 0x11, 0x22);
        bytes.truncate(RECORD_BYTES - 1);
        let topology = topology(&[("proc.4/fds", bytes)]);
        assert_eq!(
            topology.join(),
            Err(Violation::TruncatedInventory {
                group: 4,
                bytes: RECORD_BYTES - 1,
            })
        );
    }

    #[test]
    fn descriptors_that_are_not_sockets_are_not_joined() {
        let topology = topology(&[("proc.1/fds", record(4, 3, 0, 0x99, 0))]);
        assert_eq!(topology.join(), Ok(0));
        assert_eq!(SocketTopology::default().join(), Ok(0));
    }

    #[test]
    fn only_descriptor_inventories_are_observed() {
        let topology = topology(&[
            ("proc.1/pages", socket(10, 0x11, 0x22)),
            ("socket-state.0000000000000011", socket(10, 0x11, 0x22)),
            ("MANIFEST", socket(10, 0x11, 0x22)),
        ]);
        assert_eq!(topology.join(), Ok(0));
    }
}
