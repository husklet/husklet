use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::sync::Arc;

use hl_descriptor::{DescriptionRef, StatusFlags};
use hl_network::{
    AddressFamily, NetworkCatalog, NetworkResourceKey, SocketHostIo, SocketId, SocketProtocol, SocketSnapshot,
    SocketState, SocketType,
};
use hl_runtime::{
    CreatedSocket, DescriptorTransfer, HostImport, ImportedTransfer, RuntimeNetworkError, RuntimeSocketRegistry,
};

use super::Native;
use crate::ffi::linux::file_transfer::FileTransferRegistry;

pub(super) struct NativeTransfer {
    host: Arc<Native>,
    sockets: Arc<RuntimeSocketRegistry<Native>>,
    catalog: Arc<NetworkCatalog>,
    files: Arc<FileTransferRegistry>,
}

struct ImportBatch<'host> {
    host: &'host Native,
    imports: Vec<HostImport<u64>>,
    active: bool,
}

impl<'host> ImportBatch<'host> {
    fn new(host: &'host Native, capacity: usize) -> Self {
        Self {
            host,
            imports: Vec::with_capacity(capacity),
            active: true,
        }
    }

    fn stage(&mut self, attachment: OwnedFd) -> Result<(), RuntimeNetworkError> {
        let (snapshot, status) = NativeTransfer::inspect(&attachment)?.ok_or(RuntimeNetworkError::Unsupported)?;
        let token = self.host.insert(attachment.into_raw_fd())?;
        let Some(resource) = NetworkResourceKey::new(token) else {
            self.host.close(token);
            return Err(RuntimeNetworkError::NoMemory);
        };
        self.imports.push(HostImport {
            created: CreatedSocket {
                token,
                resource,
                binding: Arc::new(()),
            },
            snapshot,
            status,
        });
        Ok(())
    }

    fn finish(mut self) -> Vec<HostImport<u64>> {
        self.active = false;
        std::mem::take(&mut self.imports)
    }
}

impl Drop for ImportBatch<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        for imported in self.imports.drain(..) {
            self.host.close(imported.created.token);
        }
    }
}

impl NativeTransfer {
    pub(super) fn new(
        host: Arc<Native>,
        sockets: Arc<RuntimeSocketRegistry<Native>>,
        catalog: Arc<NetworkCatalog>,
        files: Arc<FileTransferRegistry>,
    ) -> Self {
        Self {
            host,
            sockets,
            catalog,
            files,
        }
    }

    fn duplicate(&self, description: &DescriptionRef) -> Result<OwnedFd, RuntimeNetworkError> {
        let token = self
            .sockets
            .host_token(description)
            .ok_or(RuntimeNetworkError::Unsupported)?;
        let source = self.host.descriptor(token)?;
        // SAFETY: F_DUPFD_CLOEXEC duplicates a live reactor-owned descriptor and returns independent ownership.
        let descriptor = unsafe { libc::fcntl(source, libc::F_DUPFD_CLOEXEC, 0) };
        if descriptor < 0 {
            return Err(Native::runtime_error());
        }
        // SAFETY: successful F_DUPFD_CLOEXEC returned one uniquely owned descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
    }

    fn inspect(descriptor: &OwnedFd) -> Result<Option<(SocketSnapshot, StatusFlags)>, RuntimeNetworkError> {
        let raw = descriptor.as_raw_fd();
        let mut kind = 0_i32;
        let mut kind_length = std::mem::size_of::<i32>() as libc::socklen_t;
        // SAFETY: kind is writable for kind_length and getsockopt retains no pointer.
        if unsafe {
            libc::getsockopt(
                raw,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                std::ptr::from_mut(&mut kind).cast(),
                &raw mut kind_length,
            )
        } != 0
        {
            let error = std::io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::ENOTSOCK) {
                Ok(None)
            } else {
                Err(Native::runtime_error())
            };
        }
        let socket_type = match kind {
            libc::SOCK_STREAM => SocketType::Stream,
            libc::SOCK_DGRAM => SocketType::Datagram,
            libc::SOCK_SEQPACKET => SocketType::SequencePacket,
            _ => return Err(RuntimeNetworkError::Unsupported),
        };
        // SAFETY: zero is valid initialization for sockaddr storage.
        let mut local = unsafe { std::mem::zeroed::<libc::sockaddr_storage>() };
        let mut local_length = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        // SAFETY: local is writable for local_length and getsockname retains no pointer.
        if unsafe { libc::getsockname(raw, std::ptr::from_mut(&mut local).cast(), &raw mut local_length) } != 0 {
            return Err(Native::runtime_error());
        }
        let local = Native::decode_address(&local, local_length)?;
        let family = match local {
            hl_network::SocketAddress::Unix(_) => AddressFamily::Unix,
            hl_network::SocketAddress::Inet4 { .. } => AddressFamily::Inet4,
            hl_network::SocketAddress::Inet6 { .. } => AddressFamily::Inet6,
        };
        // SAFETY: zero is valid initialization for sockaddr storage.
        let mut peer = unsafe { std::mem::zeroed::<libc::sockaddr_storage>() };
        let mut peer_length = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        // SAFETY: peer is writable for peer_length and getpeername retains no pointer.
        let peer = (unsafe { libc::getpeername(raw, std::ptr::from_mut(&mut peer).cast(), &raw mut peer_length) } == 0)
            .then(|| Native::decode_address(&peer, peer_length))
            .transpose()?;
        // SAFETY: F_GETFL reads flags without changing descriptor ownership.
        let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
        if flags < 0 {
            return Err(Native::runtime_error());
        }
        let access = match flags & libc::O_ACCMODE {
            libc::O_WRONLY => 1,
            libc::O_RDWR => 2,
            _ => 0,
        };
        let status = StatusFlags::from_bits(
            access
                | if flags & libc::O_NONBLOCK != 0 {
                    StatusFlags::NONBLOCKING
                } else {
                    0
                },
        );
        Ok(Some((
            SocketSnapshot {
                id: SocketId { slot: 1, generation: 1 },
                family,
                socket_type,
                protocol: match family {
                    AddressFamily::Unix => SocketProtocol::Default,
                    _ if socket_type == SocketType::Datagram => SocketProtocol::Udp,
                    _ => SocketProtocol::Tcp,
                },
                state: if peer.is_some() {
                    SocketState::Connected
                } else {
                    SocketState::Bound
                },
                local: Some(local),
                peer,
                connect_error: None,
                nonblocking: flags & libc::O_NONBLOCK != 0,
                shutdown: Default::default(),
            },
            status,
        )))
    }

    fn import_socket(&self, attachment: OwnedFd) -> Result<ImportedTransfer, RuntimeNetworkError> {
        let mut batch = ImportBatch::new(&self.host, 1);
        batch.stage(attachment)?;
        self.sockets
            .import_hosts(Arc::clone(&self.host), Arc::clone(&self.catalog), batch.finish())
    }
}

impl DescriptorTransfer<OwnedFd> for NativeTransfer {
    fn export(&self, description: &DescriptionRef) -> Result<OwnedFd, RuntimeNetworkError> {
        match self.duplicate(description) {
            Ok(descriptor) => Ok(descriptor),
            Err(RuntimeNetworkError::Unsupported) => self.files.duplicate(description.description_identity()),
            Err(error) => Err(error),
        }
    }

    fn import(&self, attachments: Vec<OwnedFd>) -> Result<ImportedTransfer, RuntimeNetworkError> {
        let mut imported = Vec::with_capacity(attachments.len());
        for attachment in attachments {
            if Self::inspect(&attachment)?.is_some() {
                imported.push(self.import_socket(attachment)?);
            } else {
                imported.push(self.files.import(attachment)?);
            }
        }
        Ok(ImportedTransfer::merge(imported))
    }
}
