//! Switch publication paths and reservations for the native network host.

use std::sync::Arc;

use hl_network::SocketAddress;
use hl_runtime::RuntimeNetworkError;

use super::Native;

impl Native {
    pub(super) fn switch_path(
        interface: &hl_network::EgressInterface,
        address: [u8; 4],
        port: u16,
    ) -> Result<(Vec<u8>, Vec<u8>), RuntimeNetworkError> {
        let bridge = Self::switch_bridge(interface)?;
        let directory = format!("/tmp/.hl-bridge-{bridge}").into_bytes();
        let path = Self::switch_destination_path_for_bridge(bridge, address, port)?;
        Ok((directory, path))
    }

    pub(super) fn switch_destination_path(
        interface: &hl_network::EgressInterface,
        address: [u8; 4],
        port: u16,
    ) -> Result<Vec<u8>, RuntimeNetworkError> {
        let bridge = Self::switch_bridge(interface)?;
        Self::switch_destination_path_for_bridge(bridge, address, port)
    }

    pub(super) fn switch_bridge(interface: &hl_network::EgressInterface) -> Result<&str, RuntimeNetworkError> {
        if interface.bridge.is_empty()
            || interface.bridge.len() > 40
            || interface
                .bridge
                .iter()
                .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(RuntimeNetworkError::Invalid);
        }
        std::str::from_utf8(&interface.bridge).map_err(|_| RuntimeNetworkError::Invalid)
    }

    pub(super) fn switch_destination_path_for_bridge(
        bridge: &str,
        address: [u8; 4],
        port: u16,
    ) -> Result<Vec<u8>, RuntimeNetworkError> {
        Self::switch_named_path(
            bridge,
            &format!("{}.{}.{}.{}:{port}", address[0], address[1], address[2], address[3]),
        )
    }

    /// The loopback rendezvous a wildcard bind publishes. Keying it by the owning interface
    /// address keeps sibling containers on one bridge from sharing a namespace-private name.
    pub(super) fn switch_loopback_path(
        interface: &hl_network::EgressInterface,
        port: u16,
    ) -> Result<Vec<u8>, RuntimeNetworkError> {
        let bridge = Self::switch_bridge(interface)?;
        let owner = interface.ipv4;
        Self::switch_named_path(
            bridge,
            &format!("lo-{}.{}.{}.{}:{port}", owner[0], owner[1], owner[2], owner[3]),
        )
    }

    pub(super) fn switch_named_path(bridge: &str, name: &str) -> Result<Vec<u8>, RuntimeNetworkError> {
        let path = format!("/tmp/.hl-bridge-{bridge}/{name}").into_bytes();
        if path.contains(&0)
            || path.len() >= size_of::<libc::sockaddr_un>() - std::mem::offset_of!(libc::sockaddr_un, sun_path)
        {
            return Err(RuntimeNetworkError::Invalid);
        }
        Ok(path)
    }

    /// Holds this interface's switch directory and returns it with the rendezvous pathname it owns.
    pub(super) fn switch_reservation(
        interface: &hl_network::EgressInterface,
        port: u16,
        ipv6_only: bool,
    ) -> Result<(Arc<hl_fs::Anchor>, Vec<u8>), RuntimeNetworkError> {
        let (directory, mut path) = Self::switch_path(interface, interface.ipv4, port)?;
        if ipv6_only {
            path.extend_from_slice(b".v6only");
        }
        Ok((Self::switch_anchor(&directory)?, path))
    }

    /// Creates the switch directory when absent and holds it, so a later rename of its pathname
    /// cannot redirect any operation on the rendezvous names inside it.
    pub(super) fn switch_anchor(directory: &[u8]) -> Result<Arc<hl_fs::Anchor>, RuntimeNetworkError> {
        use std::os::unix::ffi::OsStrExt;
        let path = std::path::Path::new(std::ffi::OsStr::from_bytes(directory));
        hl_fs::Anchor::create(path, 0o700).map_err(Self::publication_error)
    }

    pub(super) fn switch_name(path: &[u8]) -> Result<&[u8], RuntimeNetworkError> {
        let separator = path
            .iter()
            .rposition(|byte| *byte == b'/')
            .ok_or(RuntimeNetworkError::Invalid)?;
        let name = &path[separator + 1..];
        if name.is_empty() {
            return Err(RuntimeNetworkError::Invalid);
        }
        Ok(name)
    }

    pub(super) fn publication_error(error: std::io::Error) -> RuntimeNetworkError {
        match error.raw_os_error() {
            Some(code) => Self::error_for(code),
            None => RuntimeNetworkError::Failed,
        }
    }

    /// An ICMP socket is answered by the emulated responder, so it never routes onto the switch.
    pub(super) fn switch_source(path: &[u8]) -> Option<SocketAddress> {
        let name = path.rsplit(|byte| *byte == b'/').next()?;
        let colon = name.iter().rposition(|byte| *byte == b':')?;
        let address = &name[..colon];
        let port = std::str::from_utf8(&name[colon + 1..]).ok()?.parse().ok()?;
        let mut ipv4 = [0_u8; 4];
        let mut octets = address.split(|byte| *byte == b'.');
        for octet in &mut ipv4 {
            *octet = std::str::from_utf8(octets.next()?).ok()?.parse().ok()?;
        }
        if octets.next().is_some() {
            return None;
        }
        Some(SocketAddress::Inet4 { address: ipv4, port })
    }

    pub(super) fn binding(&self, address: &SocketAddress) -> Option<SocketAddress> {
        self.shared
            .bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|(guest, _)| match (guest, address) {
                (
                    SocketAddress::Inet4 {
                        address: bound,
                        port: bound_port,
                    },
                    SocketAddress::Inet4 {
                        address: target,
                        port: target_port,
                    },
                ) => bound_port == target_port && (bound == target || *bound == [0; 4]),
                _ => guest == address,
            })
            .map(|(_, host)| host.clone())
    }
}
