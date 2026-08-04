use crate::{
    AccessIdentity, GuestPathBytes, Kind, MutationAction, MutationError, Permissions, Umask, VfsMutationHost,
    VfsMutations, WatchEvent,
};

impl<H: VfsMutationHost> VfsMutations<'_, H> {
    pub fn mkdir(
        &self,
        path: &GuestPathBytes,
        requested: Permissions,
        identity: &AccessIdentity,
        umask: &Umask,
    ) -> Result<Vec<WatchEvent>, MutationError> {
        self.create(path, Kind::Directory, umask.apply(requested), 0, identity)
    }

    pub fn mknod(
        &self,
        path: &GuestPathBytes,
        kind: Kind,
        requested: Permissions,
        device: u64,
        identity: &AccessIdentity,
        umask: &Umask,
    ) -> Result<Vec<WatchEvent>, MutationError> {
        if !matches!(
            kind,
            Kind::Regular | Kind::Character | Kind::Block | Kind::Fifo | Kind::Socket
        ) {
            return Err(MutationError::InvalidArgument);
        }
        self.create(path, kind, umask.apply(requested), device, identity)
    }

    pub fn symlink(
        &self,
        target: &GuestPathBytes,
        path: &GuestPathBytes,
        identity: &AccessIdentity,
    ) -> Result<Vec<WatchEvent>, MutationError> {
        if target.as_bytes().is_empty() {
            return Err(MutationError::InvalidName);
        }
        self.ensure_writable(path)?;
        let parent = self.resolve(path, true, true)?;
        self.check_parent(parent.parent(), identity)?;
        self.ensure_absent(parent.parent(), Self::name(&parent)?)?;
        let action = MutationAction::Symlink {
            parent: parent.parent(),
            name: Self::name(&parent)?.clone(),
            target: target.clone(),
        };
        self.publish(&[Self::pin(&parent)], &[action])?;
        Ok(vec![WatchEvent::Created {
            path: path.clone(),
            directory: false,
        }])
    }

    pub fn link(
        &self,
        source: &GuestPathBytes,
        target: &GuestPathBytes,
        follow_source: bool,
        identity: &AccessIdentity,
    ) -> Result<Vec<WatchEvent>, MutationError> {
        self.ensure_pair_writable(source, target)?;
        let old = self.resolve(source, !follow_source, false)?;
        let new = self.resolve(target, true, true)?;
        self.check_parent(new.parent(), identity)?;
        self.ensure_present(old.parent(), Self::name(&old)?, !follow_source)?;
        self.ensure_absent(new.parent(), Self::name(&new)?)?;
        let mut actions = Vec::new();
        self.copy_up(&mut actions, old.parent(), Self::name(&old)?, false);
        actions.push(MutationAction::HardLink {
            source_parent: old.parent(),
            source_name: Self::name(&old)?.clone(),
            target_parent: new.parent(),
            target_name: Self::name(&new)?.clone(),
            follow_source,
        });
        self.publish(&[Self::pin(&old), Self::pin(&new)], &actions)?;
        Ok(vec![WatchEvent::Created {
            path: target.clone(),
            directory: false,
        }])
    }
}
