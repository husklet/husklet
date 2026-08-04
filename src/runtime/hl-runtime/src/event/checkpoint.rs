use std::sync::Arc;

use hl_descriptor::DescriptionIdentity;
use hl_event::{
    EpollTargetCheckpoint, EpollWatchSnapshot, EventObjectId, EventResourceKey, Inotify, InotifyWatchCheckpoint,
};
use hl_linux::GuestMemory;

use super::RuntimeEventSyscalls;
use super::catalog::CatalogBoundEvent;
use crate::{DescriptorReference, EventObjectBindings};

impl<M: GuestMemory> RuntimeEventSyscalls<M> {
    fn epoll_descriptor_key(key: hl_event::EpollWatchKey) -> Result<EventResourceKey, ()> {
        let number = u32::try_from(key.descriptor_number).map_err(|_| ())?;
        EventResourceKey::new(((u64::from(number) + 1) << 32) | u64::from(key.descriptor_generation)).ok_or(())
    }

    pub(super) fn add_epoll_checkpoint(&self, descriptor: i32, key: hl_event::EpollWatchKey) -> Result<(), ()> {
        let Some((bindings, _)) = &self.checkpoint else {
            return Ok(());
        };
        let source = self.descriptors.pin(descriptor).map_err(|_| ())?;
        let id = bindings.object_id(source.description_identity().identity).ok_or(())?;
        let resource = Self::epoll_descriptor_key(key)?;
        bindings
            .register_descriptor(
                resource,
                DescriptorReference {
                    number: key.descriptor_number,
                    generation: key.descriptor_generation,
                },
            )
            .map_err(|_| ())?;
        let nested = bindings
            .object_id(key.description.identity)
            .filter(|target| self.catalog.with_epoll(*target, |_| ()).is_ok());
        let watch = self
            .catalog
            .with_epoll(id, |epoll| epoll.watch_count().checked_sub(1))
            .map_err(|_| ())?
            .ok_or(())?;
        if self
            .catalog
            .add_epoll_target(
                id,
                EpollTargetCheckpoint {
                    watch,
                    descriptor: resource,
                    nested,
                },
            )
            .is_ok()
        {
            return Ok(());
        }
        // Closing a watched open-file description removes its epoll watch
        // asynchronously. Rebuild only on that uncommon divergence so the
        // checkpoint image never retains the retired watch while ordinary
        // EPOLL_CTL_ADD remains incremental.
        self.refresh_epoll_checkpoint(descriptor)
    }

    pub(super) fn remove_epoll_checkpoint(&self, descriptor: i32, key: hl_event::EpollWatchKey) -> Result<(), ()> {
        let Some((bindings, _)) = &self.checkpoint else {
            return Ok(());
        };
        let source = self.descriptors.pin(descriptor).map_err(|_| ())?;
        let id = bindings.object_id(source.description_identity().identity).ok_or(())?;
        if self
            .catalog
            .remove_epoll_target(id, Self::epoll_descriptor_key(key)?)
            .is_ok()
        {
            return Ok(());
        }
        self.refresh_epoll_checkpoint(descriptor)
    }

    fn refresh_epoll_checkpoint(&self, descriptor: i32) -> Result<(), ()> {
        let Some((bindings, _)) = &self.checkpoint else {
            return Ok(());
        };
        let source = self.descriptors.pin(descriptor).map_err(|_| ())?;
        let id = bindings.object_id(source.description_identity().identity).ok_or(())?;
        let snapshot = self.catalog.with_epoll(id, hl_event::Epoll::snapshot).map_err(|_| ())?;
        let targets = snapshot
            .watches
            .iter()
            .enumerate()
            .map(|(watch, saved)| self.epoll_target(bindings, watch, saved))
            .collect::<Result<Vec<_>, _>>()?;
        self.catalog.replace_epoll_targets(id, targets).map_err(|_| ())
    }

    fn epoll_target(
        &self,
        bindings: &EventObjectBindings,
        watch: usize,
        saved: &EpollWatchSnapshot,
    ) -> Result<EpollTargetCheckpoint, ()> {
        let descriptor = Self::epoll_descriptor_key(saved.key)?;
        bindings
            .register_descriptor(
                descriptor,
                DescriptorReference {
                    number: saved.key.descriptor_number,
                    generation: saved.key.descriptor_generation,
                },
            )
            .map_err(|_| ())?;
        let nested = bindings
            .object_id(saved.key.description.identity)
            .filter(|target| self.catalog.with_epoll(*target, |_| ()).is_ok());
        Ok(EpollTargetCheckpoint {
            watch,
            descriptor,
            nested,
        })
    }

    pub(super) fn bind_checkpoint(
        &self,
        bound: &Arc<CatalogBoundEvent>,
        identity: DescriptionIdentity,
        id: EventObjectId,
    ) -> Result<(), ()> {
        let Some((bindings, _)) = &self.checkpoint else {
            return Ok(());
        };
        bound.bind_checkpoint(bindings.clone(), identity, id).map_err(|_| ())
    }

    pub(super) fn refresh_inotify_checkpoint(&self, identity: DescriptionIdentity, object: &Inotify) -> Result<(), ()> {
        let Some((bindings, _)) = &self.checkpoint else {
            return Ok(());
        };
        let id = bindings.object_id(identity.identity).ok_or(())?;
        let source = self.catalog.inotify_source(id).map_err(|_| ())?;
        let watches = object
            .snapshot()
            .watches
            .iter()
            .enumerate()
            .map(|(watch, _)| InotifyWatchCheckpoint { watch, source })
            .collect();
        self.catalog.replace_inotify_watches(id, watches).map_err(|_| ())
    }
}
