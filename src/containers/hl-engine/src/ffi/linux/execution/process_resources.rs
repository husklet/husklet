use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use hl_descriptor::DescriptorTable;
use hl_runtime::{ProcfsError, WorkingDirectory};
use hl_task::ProcessId;

struct Resources {
    descriptors: Weak<DescriptorTable>,
    working: Weak<WorkingDirectory>,
}

/// Instance-owned publication of process filesystem resources for procfs.
pub(super) struct Catalog(Mutex<BTreeMap<ProcessId, Resources>>);

impl Catalog {
    pub(super) fn new(
        process: ProcessId,
        descriptors: &Arc<DescriptorTable>,
        working: &Arc<WorkingDirectory>,
    ) -> Arc<Self> {
        let catalog = Arc::new(Self(Mutex::new(BTreeMap::new())));
        catalog.publish(process, descriptors, working);
        catalog
    }

    pub(super) fn publish(
        &self,
        process: ProcessId,
        descriptors: &Arc<DescriptorTable>,
        working: &Arc<WorkingDirectory>,
    ) {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(
            process,
            Resources {
                descriptors: Arc::downgrade(descriptors),
                working: Arc::downgrade(working),
            },
        );
    }

    fn resolve<T>(
        &self,
        process: ProcessId,
        select: impl FnOnce(&Resources) -> Option<Arc<T>>,
    ) -> Result<Arc<T>, ProcfsError> {
        let mut entries = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(value) = entries.get(&process).and_then(select) else {
            entries.remove(&process);
            return Err(ProcfsError::NotFound);
        };
        Ok(value)
    }
}

impl hl_runtime::ProcfsResourcePort for Catalog {
    fn descriptors(&self, process: ProcessId) -> Result<Arc<DescriptorTable>, ProcfsError> {
        self.resolve(process, |resources| resources.descriptors.upgrade())
    }

    fn working(&self, process: ProcessId) -> Result<Arc<WorkingDirectory>, ProcfsError> {
        self.resolve(process, |resources| resources.working.upgrade())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_tracks_owner_lifetime() {
        let process = ProcessId::from_wire(7, 1).unwrap();
        let descriptors = Arc::new(DescriptorTable::new(4).unwrap());
        let working = Arc::new(WorkingDirectory::root());
        let catalog = Catalog::new(process, &descriptors, &working);
        assert!(hl_runtime::ProcfsResourcePort::descriptors(catalog.as_ref(), process).is_ok());
        assert!(hl_runtime::ProcfsResourcePort::working(catalog.as_ref(), process).is_ok());
        drop(descriptors);
        assert!(matches!(
            hl_runtime::ProcfsResourcePort::descriptors(catalog.as_ref(), process),
            Err(ProcfsError::NotFound)
        ));
        assert!(matches!(
            hl_runtime::ProcfsResourcePort::working(catalog.as_ref(), process),
            Err(ProcfsError::NotFound)
        ));
    }
}
