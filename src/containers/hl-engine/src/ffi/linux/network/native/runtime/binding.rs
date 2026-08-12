use std::sync::Arc;

use hl_network::{BindRoute, SocketAddress};
use hl_runtime::{RuntimeNetworkError, RuntimeNetworkHost};

use super::{Native, SwitchPath};

struct SwitchAliases<'a> {
    anchor: &'a Arc<hl_fs::Anchor>,
    path: &'a [u8],
    interface: &'a hl_network::EgressInterface,
    aliases: &'a [hl_network::EgressInterface],
    port: u16,
    ipv6_only: bool,
    wildcard_stream: bool,
}

impl Native {
    pub(super) fn bind_switch_route(&self, token: u64, route: BindRoute) -> Result<SocketAddress, RuntimeNetworkError> {
        if route.aliases.len() > hl_network::BIND_ROUTE_ALIAS_MAXIMUM {
            return Err(RuntimeNetworkError::Invalid);
        }
        let Some(interface) = route.interface else {
            return self.bind(token, route.address);
        };
        let (address, port, ipv6_wildcard) = match route.address {
            SocketAddress::Inet4 { address, port } => (address, port, false),
            SocketAddress::Inet6 { address, port, .. } if address == [0; 16] => ([0; 4], port, true),
            SocketAddress::Inet6 { .. } | SocketAddress::Unix(_) => return Err(RuntimeNetworkError::Invalid),
        };
        let kind = self.socket_type(token)?;
        if !matches!(kind, libc::SOCK_STREAM | libc::SOCK_DGRAM) {
            return Err(RuntimeNetworkError::OperationNotSupported);
        }
        let first = if port == 0 {
            20_000_u16.wrapping_add((token as u16) & 0x3fff)
        } else {
            port
        };
        let attempts = if port == 0 { 45_000 } else { 1 };
        let ipv6_only = if ipv6_wildcard {
            matches!(
                super::super::super::socket_option::get(self.descriptor(token)?, 41, 26),
                Ok(hl_linux::GuestSocketOption::Scalar(value)) if value != 0
            )
        } else {
            false
        };
        let mut descriptor = None;
        for offset in 0..attempts {
            let candidate = first.wrapping_add(offset as u16).max(1024);
            let bound = if let Some(bound) = descriptor {
                bound
            } else {
                let value = self.switch_socket(token, kind)?;
                descriptor = Some(value);
                value
            };
            let mut publication = hl_fs::Publication::default();
            let staged = (|| {
                let (anchor, path) = Self::switch_reservation(&interface, candidate, ipv6_only)?;
                let (storage, length) = Self::socket_address(&SocketAddress::Unix(path.clone()))?;
                // SAFETY: storage contains a bounded sockaddr_un and descriptor remains table-owned.
                if unsafe { libc::bind(bound, (&raw const storage).cast(), length) } != 0 {
                    return Err(Self::runtime_error());
                }
                // The peer protocol reads this name back with getsockname, so the bind must address it
                // by pathname; adoption confirms the entry it created is the one the anchor holds.
                publication
                    .adopt(&anchor, Self::switch_name(&path)?, path.clone())
                    .map_err(Self::publication_error)?;
                Self::stage_switch_aliases(
                    &mut publication,
                    SwitchAliases {
                        anchor: &anchor,
                        path: &path,
                        interface: &interface,
                        aliases: &route.aliases,
                        port: candidate,
                        ipv6_only,
                        wildcard_stream: address == [0; 4] && kind == libc::SOCK_STREAM,
                    },
                )?;
                publication.commit().map_err(Self::publication_error)
            })();
            if let Err(error) = staged {
                drop(publication);
                if port != 0 || error != RuntimeNetworkError::AddressInUse {
                    self.restore_inet_socket(token, kind)?;
                    return Err(error);
                }
                continue;
            }
            let local = SocketAddress::Inet4 {
                address: interface.ipv4,
                port: candidate,
            };
            let mut sockets = self
                .shared
                .sockets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
            entry.guest_local = Some(local.clone());
            entry.switch_interface = Some(interface.clone());
            let ownership = Arc::new(SwitchPath::new(publication));
            let weak = Arc::downgrade(&ownership);
            let mut registry = self
                .shared
                .switch_paths
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for owned_path in ownership.names() {
                registry.insert(owned_path.clone(), weak.clone());
            }
            entry.switch_path = Some(ownership);
            return Ok(local);
        }
        if descriptor.is_some() {
            self.restore_inet_socket(token, kind)?;
        }
        Err(RuntimeNetworkError::AddressInUse)
    }

    fn stage_switch_aliases(
        publication: &mut hl_fs::Publication,
        aliases: SwitchAliases<'_>,
    ) -> Result<(), RuntimeNetworkError> {
        if !aliases.wildcard_stream {
            return Ok(());
        }
        for alias in aliases.aliases {
            let (alias_anchor, alias_path) = Self::switch_reservation(alias, aliases.port, aliases.ipv6_only)?;
            let alias_name = Self::switch_name(&alias_path)?.to_vec();
            publication
                .reserve_link(&alias_anchor, &alias_name, alias_path, aliases.path)
                .map_err(Self::publication_error)?;
        }
        if aliases.ipv6_only {
            return Ok(());
        }
        let loopback = Self::switch_loopback_path(aliases.interface, aliases.port)?;
        let loopback_name = Self::switch_name(&loopback)?.to_vec();
        publication
            .reserve_link(aliases.anchor, &loopback_name, loopback, aliases.path)
            .map_err(Self::publication_error)
    }
}
