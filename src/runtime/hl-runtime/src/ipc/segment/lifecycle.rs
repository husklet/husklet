use std::sync::Arc;

use hl_ipc::{CommittedMemoryFork, IpcCatalog, OwnedPreparedMemoryFork, SharedMemoryError as CatalogError};
use hl_memory::MappingHost;

use crate::{
    CommittedBindingSet, ForkBinding, MappingError, MemoryBinding, MemoryMappings, MemoryPort, PreparedBindingSet,
};

/// Joins process mapping bindings to namespace attachment accounting.
///
/// Fork/exec/exit coordinators call this capability inside their existing
/// outer transaction, after the child mapping ledger exists and before a new
/// image or terminal process state is published.
pub struct MemoryLifecycle {
    catalog: Arc<IpcCatalog>,
    mappings: Arc<dyn MemoryPort>,
}

pub struct PreparedFork<'a> {
    namespace: OwnedPreparedMemoryFork,
    mappings: Box<dyn PreparedBindingSet<'a> + 'a>,
}

pub struct CommittedFork<'a> {
    namespace: CommittedMemoryFork,
    mappings: Box<dyn CommittedBindingSet + 'a>,
}

pub(crate) struct OwnedPreparedFork {
    namespace: OwnedPreparedMemoryFork,
    mappings: super::OwnedPreparedBindings,
}

pub(crate) struct OwnedCommittedFork {
    namespace: CommittedMemoryFork,
    mappings: super::OwnedCommittedBindings,
}

impl OwnedPreparedFork {
    pub(crate) fn commit(self) -> Result<OwnedCommittedFork, MappingError> {
        let mappings = self.mappings.commit()?;
        match self.namespace.commit_reversible() {
            Ok(namespace) => Ok(OwnedCommittedFork { namespace, mappings }),
            Err(error) => {
                mappings.rollback()?;
                Err(MemoryLifecycle::domain_error(error))
            }
        }
    }
}

impl OwnedCommittedFork {
    pub(crate) fn rollback(self) -> Result<(), MappingError> {
        self.namespace.rollback().map_err(MemoryLifecycle::domain_error)?;
        self.mappings.rollback()
    }

    pub(crate) fn finish(self) {
        self.namespace.finish();
        self.mappings.finish();
    }
}

impl CommittedFork<'_> {
    pub fn rollback(self) -> Result<(), MappingError> {
        self.namespace.rollback().map_err(MemoryLifecycle::domain_error)?;
        self.mappings.rollback()
    }

    pub fn finish(self) {
        self.namespace.finish();
        self.mappings.finish();
    }
}

impl<'a> PreparedFork<'a> {
    pub fn commit(self) -> Result<(), MappingError> {
        self.commit_reversible().map(CommittedFork::finish)
    }

    pub fn commit_reversible(self) -> Result<CommittedFork<'a>, MappingError> {
        let mappings = self.mappings.commit()?;
        match self.namespace.commit_reversible() {
            Ok(namespace) => Ok(CommittedFork { namespace, mappings }),
            Err(error) => {
                if mappings.rollback().is_err() {
                    return Err(MappingError::Invariant);
                }
                Err(MemoryLifecycle::domain_error(error))
            }
        }
    }
}

impl MemoryLifecycle {
    #[must_use]
    pub fn new(catalog: Arc<IpcCatalog>, mappings: Arc<dyn MemoryPort>) -> Self {
        Self { catalog, mappings }
    }

    pub fn fork(&self, parent: u32, child: u32, now: u64, child_mappings: &dyn MemoryPort) -> Result<(), MappingError> {
        let inherited = self
            .catalog
            .with_shared_memory(|namespace| namespace.fork(parent, child, now))
            .map_err(Self::domain_error)?;
        let replacements = inherited
            .iter()
            .map(|binding| (binding.parent, binding.child))
            .collect::<Vec<_>>();
        let mut bindings = self.mappings.bindings()?;
        for binding in &mut bindings {
            binding.attachment = replacements
                .iter()
                .find(|(parent, _)| *parent == binding.attachment)
                .map(|(_, child)| *child)
                .ok_or(MappingError::Invariant)?;
        }
        if child_mappings.restore_bindings(&bindings).is_ok() {
            return Ok(());
        }
        let rollback = self.catalog.with_shared_memory(|namespace| {
            for binding in inherited {
                namespace.shmdt(binding.child, child, now).map_err(Self::domain_error)?;
            }
            Ok::<(), MappingError>(())
        });
        if rollback.is_err() {
            return Err(MappingError::Invariant);
        }
        Err(MappingError::Invariant)
    }

    pub fn prepare_fork<'a>(
        &'a self,
        parent: u32,
        child: u32,
        now: u64,
        child_mappings: &'a dyn MemoryPort,
    ) -> Result<PreparedFork<'a>, MappingError> {
        let namespace = self
            .catalog
            .shared_memory()
            .prepare_fork_owned(parent, child, now)
            .map_err(Self::domain_error)?;
        let inherited = namespace.bindings();
        let bindings = self.mappings.bindings()?;
        if inherited.len() != bindings.len() {
            return Err(MappingError::Invariant);
        }
        let mut planned = Vec::with_capacity(bindings.len());
        for binding in bindings {
            let replacement = inherited
                .iter()
                .find(|candidate| candidate.parent == binding.attachment)
                .ok_or(MappingError::Invariant)?;
            planned.push(ForkBinding {
                binding: MemoryBinding {
                    attachment: replacement.child,
                    ..binding
                },
                backing: replacement.backing,
            });
        }
        let mappings = child_mappings.prepare_fork_bindings(&planned)?;
        Ok(PreparedFork { namespace, mappings })
    }

    pub(crate) fn prepare_owned_fork<H: MappingHost>(
        &self,
        parent: u32,
        child: u32,
        now: u64,
        child_mappings: &MemoryMappings<H>,
    ) -> Result<OwnedPreparedFork, MappingError> {
        let namespace = self
            .catalog
            .shared_memory()
            .prepare_fork_owned(parent, child, now)
            .map_err(Self::domain_error)?;
        let inherited = namespace.bindings();
        let bindings = self.mappings.bindings()?;
        if inherited.len() != bindings.len() {
            return Err(MappingError::Invariant);
        }
        let mut planned = Vec::with_capacity(bindings.len());
        for binding in bindings {
            let replacement = inherited
                .iter()
                .find(|candidate| candidate.parent == binding.attachment)
                .ok_or(MappingError::Invariant)?;
            planned.push(ForkBinding {
                binding: MemoryBinding {
                    attachment: replacement.child,
                    ..binding
                },
                backing: replacement.backing,
            });
        }
        let mappings = child_mappings.prepare_owned_bindings(&planned)?;
        Ok(OwnedPreparedFork { namespace, mappings })
    }

    pub fn detach_process(&self, process: u32, now: u64) -> Result<(), MappingError> {
        self.mappings.unmap_all()?;
        self.catalog
            .with_shared_memory(|namespace| namespace.exit(process, now))
            .map_err(Self::domain_error)
    }

    pub fn checkpoint_bindings(&self) -> Result<Vec<MemoryBinding>, MappingError> {
        self.mappings.bindings()
    }

    fn domain_error(error: CatalogError) -> MappingError {
        match error {
            CatalogError::ResourceLimit => MappingError::NoMemory,
            CatalogError::InvalidArgument | CatalogError::NotFound | CatalogError::Permission => MappingError::Invalid,
            _ => MappingError::Invariant,
        }
    }
}
