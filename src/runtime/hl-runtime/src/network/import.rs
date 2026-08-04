use std::sync::Arc;

use hl_descriptor::{DescriptionIdentity, OpenFileDescription, StatusFlags};
use hl_network::{NetworkCatalog, SocketDescription, SocketSnapshot};

use crate::{
    CreatedSocket, ImportedDescription, ImportedTransfer, RuntimeNetworkError, RuntimeNetworkHost, RuntimeSocket,
    RuntimeSocketRegistry, TransferPublication,
};

pub struct HostImport<T> {
    pub created: CreatedSocket<T>,
    pub snapshot: SocketSnapshot,
    pub status: StatusFlags,
}

impl<H: RuntimeNetworkHost> RuntimeSocketRegistry<H> {
    pub fn import_hosts(
        self: &Arc<Self>,
        host: Arc<H>,
        catalog: Arc<NetworkCatalog>,
        imports: Vec<HostImport<H::Token>>,
    ) -> Result<ImportedTransfer, RuntimeNetworkError> {
        let mut transfer = SocketTransfer::new(Arc::clone(self), imports.len());
        for imported in imports {
            transfer.stage(&host, &catalog, imported)?;
        }
        let descriptions = transfer.descriptions();
        Ok(ImportedTransfer::new(descriptions, Box::new(transfer)))
    }
}

struct SocketTransfer<H: RuntimeNetworkHost> {
    registry: Arc<RuntimeSocketRegistry<H>>,
    objects: Vec<(Arc<RuntimeSocket<H>>, StatusFlags)>,
    active: bool,
}

impl<H: RuntimeNetworkHost> SocketTransfer<H> {
    fn new(registry: Arc<RuntimeSocketRegistry<H>>, capacity: usize) -> Self {
        Self {
            registry,
            objects: Vec::with_capacity(capacity),
            active: true,
        }
    }

    fn stage(
        &mut self,
        host: &Arc<H>,
        catalog: &Arc<NetworkCatalog>,
        imported: HostImport<H::Token>,
    ) -> Result<(), RuntimeNetworkError> {
        let description = Arc::new(SocketDescription::new(
            Arc::clone(host),
            imported.created.token,
            imported.status,
        ));
        description.bind_readiness();
        let mut snapshot = imported.snapshot;
        let id = catalog
            .insert_host(
                snapshot.clone(),
                imported.created.resource,
                imported.created.binding,
                Vec::new(),
            )
            .map_err(|_| {
                description.close();
                RuntimeNetworkError::NoMemory
            })?;
        snapshot.id = id;
        self.objects.push((
            RuntimeSocket::host(description, imported.created.token, id, snapshot, Arc::clone(catalog)),
            imported.status,
        ));
        Ok(())
    }

    fn descriptions(&self) -> Vec<ImportedDescription> {
        self.objects
            .iter()
            .map(|(object, status)| ImportedDescription {
                object: object.clone(),
                status: *status,
            })
            .collect()
    }

    fn close(&mut self) {
        if !self.active {
            return;
        }
        for (object, _) in &self.objects {
            OpenFileDescription::close(object.as_ref());
        }
        self.active = false;
    }
}

impl<H: RuntimeNetworkHost> TransferPublication for SocketTransfer<H> {
    fn bind(&mut self, identities: &[DescriptionIdentity]) -> Result<(), RuntimeNetworkError> {
        if identities.len() != self.objects.len() {
            return Err(RuntimeNetworkError::Invalid);
        }
        for (identity, (object, _)) in identities.iter().zip(&self.objects) {
            if self.registry.register(*identity, Arc::clone(object)).is_err() {
                return Err(RuntimeNetworkError::NoMemory);
            }
        }
        Ok(())
    }

    fn commit(mut self: Box<Self>) {
        self.active = false;
    }

    fn rollback(mut self: Box<Self>) {
        self.close();
    }
}

impl<H: RuntimeNetworkHost> Drop for SocketTransfer<H> {
    fn drop(&mut self) {
        self.close();
    }
}
