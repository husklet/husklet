use crate::{
    Access, AccessIdentity, GuestPathBytes, Kind, Metadata, MountNamespace, MountRoute, MutationAction, MutationError,
    OpenIntent, OpenPlan, Permissions, ReadOnlyPaths, RenameFlags, ResolveRequest, Resolver, TimestampUpdate, Umask,
    VfsMutationHost, WatchEvent,
};
use std::sync::atomic::{AtomicU32, Ordering};

/// Namespace mutation policy. Host pins and staged state never escape an operation.
pub struct VfsMutations<'namespace, H: VfsMutationHost> {
    pub(crate) resolver: Resolver<'namespace, H>,
    namespace: &'namespace MountNamespace,
    read_only: &'namespace ReadOnlyPaths,
    root_read_only: bool,
    overlay: bool,
    move_cookie: AtomicU32,
}

impl<'namespace, H: VfsMutationHost> VfsMutations<'namespace, H> {
    pub const fn new(
        host: H,
        namespace: &'namespace MountNamespace,
        read_only: &'namespace ReadOnlyPaths,
        root_read_only: bool,
        overlay: bool,
    ) -> Self {
        Self {
            resolver: Resolver::new(host, namespace),
            namespace,
            read_only,
            root_read_only,
            overlay,
            move_cookie: AtomicU32::new(1),
        }
    }
    pub fn unlink(&self, path: &GuestPathBytes, identity: &AccessIdentity) -> Result<Vec<WatchEvent>, MutationError> {
        self.remove(path, false, identity)
    }

    pub fn rmdir(&self, path: &GuestPathBytes, identity: &AccessIdentity) -> Result<Vec<WatchEvent>, MutationError> {
        self.remove(path, true, identity)
    }

    pub fn rename(
        &self,
        source: &GuestPathBytes,
        target: &GuestPathBytes,
        flags: RenameFlags,
        identity: &AccessIdentity,
    ) -> Result<Vec<WatchEvent>, MutationError> {
        self.ensure_pair_writable(source, target)?;
        let old = self.resolve(source, true, false)?;
        let new = self.resolve(target, true, true)?;
        let source_metadata = self.required_metadata(old.parent(), Self::name(&old)?, true)?;
        self.check_parent(old.parent(), identity)?;
        self.check_parent(new.parent(), identity)?;
        self.check_sticky(old.parent(), &source_metadata, identity)?;
        let target_metadata = self
            .resolver
            .host()
            .metadata_at(new.parent(), Self::name(&new)?, true)?;
        if flags.contains(RenameFlags::NOREPLACE) && target_metadata.is_some() {
            return Err(MutationError::AlreadyExists);
        }
        if flags.contains(RenameFlags::EXCHANGE) && target_metadata.is_none() {
            return Err(MutationError::NotFound);
        }
        if let Some(metadata) = &target_metadata {
            self.check_sticky(new.parent(), metadata, identity)?;
        }
        let mut actions = Vec::new();
        self.copy_up(&mut actions, old.parent(), Self::name(&old)?, true);
        if flags.contains(RenameFlags::EXCHANGE) {
            self.copy_up(&mut actions, new.parent(), Self::name(&new)?, true);
        }
        actions.push(MutationAction::Rename {
            source_parent: old.parent(),
            source_name: Self::name(&old)?.clone(),
            target_parent: new.parent(),
            target_name: Self::name(&new)?.clone(),
            flags,
            whiteout_lower: self.overlay || flags.contains(RenameFlags::WHITEOUT),
        });
        self.publish(&[Self::pin(&old), Self::pin(&new)], &actions)?;
        let cookie = self.next_cookie();
        Ok(vec![
            WatchEvent::MovedFrom {
                path: source.clone(),
                cookie,
            },
            WatchEvent::MovedTo {
                path: target.clone(),
                cookie,
            },
        ])
    }

    pub fn chmod(
        &self,
        path: &GuestPathBytes,
        permissions: Permissions,
        identity: &AccessIdentity,
        nofollow: bool,
    ) -> Result<Vec<WatchEvent>, MutationError> {
        self.ensure_writable(path)?;
        let parent = self.resolve(path, nofollow, false)?;
        let current = self.required_metadata(parent.parent(), Self::name(&parent)?, nofollow)?;
        let changed = identity
            .chmod(&current, permissions)
            .map_err(MutationError::from_access)?;
        self.set_metadata(path, &parent, changed, nofollow)
    }

    pub fn chown(
        &self,
        path: &GuestPathBytes,
        user: Option<u32>,
        group: Option<u32>,
        identity: &AccessIdentity,
        nofollow: bool,
    ) -> Result<Vec<WatchEvent>, MutationError> {
        self.ensure_writable(path)?;
        let parent = self.resolve(path, nofollow, false)?;
        let current = self.required_metadata(parent.parent(), Self::name(&parent)?, nofollow)?;
        let changed = identity
            .chown(&current, user, group)
            .map_err(MutationError::from_access)?;
        self.set_metadata(path, &parent, changed, nofollow)
    }

    pub fn utimens(
        &self,
        path: &GuestPathBytes,
        accessed: TimestampUpdate,
        modified: TimestampUpdate,
        identity: &AccessIdentity,
        nofollow: bool,
    ) -> Result<Vec<WatchEvent>, MutationError> {
        self.ensure_writable(path)?;
        let parent = self.resolve(path, nofollow, false)?;
        let metadata = self.required_metadata(parent.parent(), Self::name(&parent)?, nofollow)?;
        if identity.user != metadata.user && !identity.capabilities.owner_override {
            let write = Access::from_bits(Access::WRITE).map_err(MutationError::from_access)?;
            identity
                .check_access(&metadata, write)
                .map_err(MutationError::from_access)?;
        }
        let mut actions = Vec::new();
        self.copy_up(&mut actions, parent.parent(), Self::name(&parent)?, false);
        actions.push(MutationAction::SetTimes {
            parent: parent.parent(),
            name: Self::name(&parent)?.clone(),
            accessed,
            modified,
            nofollow,
        });
        self.publish(&[Self::pin(&parent)], &actions)?;
        Ok(vec![WatchEvent::Attributes { path: path.clone() }])
    }

    pub fn open(
        &self,
        plan: &OpenPlan,
        requested: Permissions,
        identity: &AccessIdentity,
        umask: &Umask,
    ) -> Result<Vec<WatchEvent>, MutationError> {
        let path = plan.path();
        let writing = plan.intent().bits()
            & (OpenIntent::WRITE | OpenIntent::CREATE | OpenIntent::TRUNCATE | OpenIntent::APPEND)
            != 0;
        if writing {
            self.ensure_writable(path)?;
        }
        let allow_missing = plan.intent().bits() & OpenIntent::CREATE != 0;
        let parent = self.resolve(path, plan.names_symlink(), allow_missing)?;
        let existing = self
            .resolver
            .host()
            .metadata_at(parent.parent(), Self::name(&parent)?, true)?;
        if existing.is_none() && !allow_missing {
            return Err(MutationError::NotFound);
        }
        if existing.is_none() {
            self.check_parent(parent.parent(), identity)?;
        }
        let permissions = umask.apply(requested);
        let action = MutationAction::Open {
            parent: parent.parent(),
            name: Self::name(&parent)?.clone(),
            plan: plan.clone(),
            permissions,
        };
        self.publish(&[Self::pin(&parent)], &[action])?;
        let mut events = Vec::new();
        if existing.is_none() {
            events.push(WatchEvent::Created {
                path: path.clone(),
                directory: false,
            });
        } else if plan.intent().bits() & OpenIntent::TRUNCATE != 0 {
            events.push(WatchEvent::Modified { path: path.clone() });
        }
        Ok(events)
    }

    pub fn truncate(
        &self,
        path: &GuestPathBytes,
        size: u64,
        identity: &AccessIdentity,
    ) -> Result<Vec<WatchEvent>, MutationError> {
        self.ensure_writable(path)?;
        let parent = self.resolve(path, false, false)?;
        let metadata = self.required_metadata(parent.parent(), Self::name(&parent)?, false)?;
        if metadata.kind == Kind::Directory {
            return Err(MutationError::IsDirectory);
        }
        let write = Access::from_bits(Access::WRITE).map_err(MutationError::from_access)?;
        identity
            .check_access(&metadata, write)
            .map_err(MutationError::from_access)?;
        let mut actions = Vec::new();
        self.copy_up(&mut actions, parent.parent(), Self::name(&parent)?, false);
        actions.push(MutationAction::Truncate {
            parent: parent.parent(),
            name: Self::name(&parent)?.clone(),
            size,
        });
        self.publish(&[Self::pin(&parent)], &actions)?;
        Ok(vec![WatchEvent::Modified { path: path.clone() }])
    }

    pub(crate) fn create(
        &self,
        path: &GuestPathBytes,
        kind: Kind,
        permissions: Permissions,
        device: u64,
        identity: &AccessIdentity,
    ) -> Result<Vec<WatchEvent>, MutationError> {
        self.ensure_writable(path)?;
        let parent = self.resolve(path, true, true)?;
        self.check_parent(parent.parent(), identity)?;
        self.ensure_absent(parent.parent(), Self::name(&parent)?)?;
        let action = MutationAction::Create {
            parent: parent.parent(),
            name: Self::name(&parent)?.clone(),
            kind,
            permissions,
            device,
        };
        self.publish(&[Self::pin(&parent)], &[action])?;
        Ok(vec![WatchEvent::Created {
            path: path.clone(),
            directory: kind == Kind::Directory,
        }])
    }

    fn remove(
        &self,
        path: &GuestPathBytes,
        directory: bool,
        identity: &AccessIdentity,
    ) -> Result<Vec<WatchEvent>, MutationError> {
        self.ensure_writable(path)?;
        let parent = self.resolve(path, true, false)?;
        let metadata = self.required_metadata(parent.parent(), Self::name(&parent)?, true)?;
        if directory != (metadata.kind == Kind::Directory) {
            return Err(if directory {
                MutationError::NotDirectory
            } else {
                MutationError::IsDirectory
            });
        }
        self.check_parent(parent.parent(), identity)?;
        self.check_sticky(parent.parent(), &metadata, identity)?;
        let action = MutationAction::Remove {
            parent: parent.parent(),
            name: Self::name(&parent)?.clone(),
            directory,
            whiteout_lower: self.overlay,
        };
        self.publish(&[Self::pin(&parent)], &[action])?;
        Ok(vec![WatchEvent::Deleted {
            path: path.clone(),
            directory,
        }])
    }

    pub(crate) fn resolve(
        &self,
        path: &GuestPathBytes,
        nofollow_final: bool,
        allow_missing_final: bool,
    ) -> Result<crate::ResolvedParent<'_, H>, MutationError> {
        let root = GuestPathBytes::new(b"/").expect("static root path");
        self.resolver
            .resolve(ResolveRequest {
                path,
                base: &root,
                nofollow_final,
                no_symlinks: false,
                allow_missing_final,
            })
            .map_err(MutationError::Resolve)
    }

    pub(crate) fn ensure_writable(&self, path: &GuestPathBytes) -> Result<(), MutationError> {
        if self
            .namespace
            .denies_write_bytes(path, self.root_read_only, self.read_only)
        {
            Err(MutationError::ReadOnly)
        } else {
            Ok(())
        }
    }

    pub(crate) fn ensure_pair_writable(
        &self,
        first: &GuestPathBytes,
        second: &GuestPathBytes,
    ) -> Result<(), MutationError> {
        self.ensure_writable(first)?;
        self.ensure_writable(second)?;
        if !Self::same_route(self.namespace.route_bytes(first), self.namespace.route_bytes(second)) {
            return Err(MutationError::CrossMount);
        }
        Ok(())
    }

    fn same_route(first: MountRoute, second: MountRoute) -> bool {
        match (first, second) {
            (MountRoute::Root, MountRoute::Root) => true,
            (MountRoute::Mounted { id: first, .. }, MountRoute::Mounted { id: second, .. }) => first == second,
            _ => false,
        }
    }

    pub(crate) fn check_parent(
        &self,
        parent: crate::NodeHandle,
        identity: &AccessIdentity,
    ) -> Result<(), MutationError> {
        let metadata = self.resolver.host().metadata_node(parent)?;
        if metadata.kind != Kind::Directory {
            return Err(MutationError::NotDirectory);
        }
        let access = Access::from_bits(Access::WRITE | Access::EXECUTE).map_err(MutationError::from_access)?;
        identity
            .check_access(&metadata, access)
            .map_err(MutationError::from_access)
    }

    /// Reference sticky policy; `VfsMutations` has no production caller, so this is the written
    /// specification rather than an enforced check. Guest-created files now record a guest owner and
    /// `setuid` now drops `CAP_FOWNER`, so both operands are meaningful; lifting this into
    /// `hl-engine` is blocked only on the pinned `owner_override`, which still short-circuits it.
    fn check_sticky(
        &self,
        parent: crate::NodeHandle,
        entry: &Metadata,
        identity: &AccessIdentity,
    ) -> Result<(), MutationError> {
        let directory = self.resolver.host().metadata_node(parent)?;
        if directory.permissions.bits() & Permissions::STICKY == 0
            || identity.user == directory.user
            || identity.user == entry.user
            || identity.capabilities.owner_override
        {
            Ok(())
        } else {
            Err(MutationError::OperationNotPermitted)
        }
    }

    fn required_metadata(
        &self,
        parent: crate::NodeHandle,
        name: &crate::GuestName,
        nofollow: bool,
    ) -> Result<Metadata, MutationError> {
        self.resolver
            .host()
            .metadata_at(parent, name, nofollow)?
            .ok_or(MutationError::NotFound)
    }

    pub(crate) fn ensure_present(
        &self,
        parent: crate::NodeHandle,
        name: &crate::GuestName,
        nofollow: bool,
    ) -> Result<(), MutationError> {
        self.required_metadata(parent, name, nofollow).map(|_| ())
    }

    pub(crate) fn ensure_absent(
        &self,
        parent: crate::NodeHandle,
        name: &crate::GuestName,
    ) -> Result<(), MutationError> {
        if self.resolver.host().metadata_at(parent, name, true)?.is_some() {
            Err(MutationError::AlreadyExists)
        } else {
            Ok(())
        }
    }

    fn set_metadata(
        &self,
        path: &GuestPathBytes,
        parent: &crate::ResolvedParent<'_, H>,
        metadata: Metadata,
        nofollow: bool,
    ) -> Result<Vec<WatchEvent>, MutationError> {
        let mut actions = Vec::new();
        self.copy_up(&mut actions, parent.parent(), Self::name(parent)?, false);
        actions.push(MutationAction::SetMetadata {
            parent: parent.parent(),
            name: Self::name(parent)?.clone(),
            metadata,
            nofollow,
        });
        self.publish(&[Self::pin(parent)], &actions)?;
        Ok(vec![WatchEvent::Attributes { path: path.clone() }])
    }

    pub(crate) fn copy_up(
        &self,
        actions: &mut Vec<MutationAction>,
        parent: crate::NodeHandle,
        name: &crate::GuestName,
        recursive: bool,
    ) {
        if self.overlay {
            actions.push(MutationAction::CopyUp {
                parent,
                name: name.clone(),
                recursive,
            });
        }
    }

    fn next_cookie(&self) -> u32 {
        loop {
            let cookie = self.move_cookie.fetch_add(1, Ordering::Relaxed);
            if cookie != 0 {
                return cookie;
            }
        }
    }
}
