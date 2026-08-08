//! `RuntimePathHost` trait adapter for the native path host.

use std::ffi::CString;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

use hl_linux::{OpenAbiPlan, PathOperand};
use hl_runtime::{
    AccessIdentity, DirectoryBaseLease, GuestPath, GuestPathBytes, OpenIntent, PreparedPathMutation, PreparedPathOpen,
    ResolveConstraints, ResolveError, ResolveRequest, ResolvedPathLease, Resolver, RuntimePathError, RuntimePathHost,
};

use super::{
    HostError, NativeFile, NativePath, attribute, device, filesystem, metadata, mutation, open, overlay_lease, pin,
    projected, resolution,
};

impl RuntimePathHost for NativePath {
    fn root_base(&self) -> Result<DirectoryBaseLease, RuntimePathError> {
        Ok(DirectoryBaseLease::root(
            GuestPath::new("/").map_err(|_| RuntimePathError::Invalid)?,
        ))
    }

    fn working_base(&self, path: GuestPath) -> Result<DirectoryBaseLease, RuntimePathError> {
        if self.source.is_projected() {
            return if path.as_str() == "/" {
                Ok(DirectoryBaseLease::root(path))
            } else {
                Err(RuntimePathError::Unsupported)
            };
        }
        let ordinary = self.ordinary()?;
        let relative = path.as_str().strip_prefix('/').ok_or(RuntimePathError::Invalid)?;
        let resolved = ordinary.root().join(relative).canonicalize().map_err(HostError::map)?;
        if !resolved.starts_with(ordinary.root()) {
            return Err(RuntimePathError::Access);
        }
        if !resolved.is_dir() {
            return Err(RuntimePathError::NotDirectory);
        }
        Ok(DirectoryBaseLease::root(path))
    }

    fn descriptor_base(&self, lease: hl_descriptor::OperationLease) -> Result<DirectoryBaseLease, RuntimePathError> {
        let metadata = lease.metadata().map_err(|_| RuntimePathError::BadDescriptor)?;
        if metadata.kind != 4 {
            return Err(RuntimePathError::NotDirectory);
        }
        let key = (metadata.device, metadata.inode);
        if let Some(path) = self
            .synthetic_paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .map(|provenance| provenance.guest.clone())
        {
            return Ok(DirectoryBaseLease::descriptor(lease, path));
        }
        if self.source.is_projected() {
            let file = self.projected.get(&key).ok_or(RuntimePathError::NotFound)?;
            return Ok(DirectoryBaseLease::descriptor(lease, file.guest()?));
        }
        let ordinary = self.ordinary()?;
        let opened = self
            .paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .cloned()
            .ok_or(RuntimePathError::NotFound)?;
        let host = ordinary.root().join(opened.guest.as_str().trim_start_matches('/'));
        let current = std::fs::metadata(host).map_err(HostError::map)?;
        if (current.dev(), current.ino()) != key {
            return Err(RuntimePathError::NotFound);
        }
        Ok(DirectoryBaseLease::descriptor(lease, opened.guest))
    }

    fn directory_path(&self, base: &DirectoryBaseLease, operand: &PathOperand) -> Result<GuestPath, RuntimePathError> {
        if operand.path.is_absolute() && !base.confines_root() {
            let path = std::str::from_utf8(operand.path.as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
            let path = GuestPath::new(path).map_err(|_| RuntimePathError::Invalid)?;
            if path.as_str() == "/" {
                return Ok(path);
            }
        }
        if let Some(context) = self.source.projected_context() {
            let node = projected::Node::resolve(context, base, operand, &self.projected)?;
            if node.metadata()?.kind != hl_runtime::FileKind::Directory {
                return Err(RuntimePathError::NotDirectory);
            }
            return projected::Path::join(context.root(), base, operand.path.as_bytes()).and_then(|path| {
                GuestPath::new(std::str::from_utf8(&path).map_err(|_| RuntimePathError::Invalid)?)
                    .map_err(|_| RuntimePathError::Invalid)
            });
        }
        let path = self.resolve_path(base, operand)?;
        if !path.is_dir() {
            return Err(RuntimePathError::NotDirectory);
        }
        self.guest_path(&path)
    }

    fn descriptor_node(
        &self,
        lease: hl_descriptor::OperationLease,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        let metadata = lease.metadata().map_err(|_| RuntimePathError::BadDescriptor)?;
        let identity = (metadata.device, metadata.inode);
        let filesystem = self
            .paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&identity)
            .map(|opened| opened.filesystem)
            .or_else(|| {
                self.synthetic_paths
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&identity)
                    .map(|provenance| provenance.filesystem)
            });
        Ok(Box::new(metadata::DescriptorNode::new(
            lease,
            filesystem,
            Arc::clone(&self.ownership),
        )))
    }

    fn filesystem(
        &self,
        base: &DirectoryBaseLease,
        operand: &PathOperand,
    ) -> Result<hl_runtime::FilesystemStats, RuntimePathError> {
        if operand.path.is_absolute() && !base.confines_root() {
            let path = std::str::from_utf8(operand.path.as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
            let guest = hl_runtime::GuestPath::new(path).map_err(|_| RuntimePathError::Invalid)?;
            let ordinary = self.ordinary()?;
            filesystem::HostFilesystem::read(ordinary.root(), &guest)
        } else {
            self.resolve(base, operand)?.filesystem()
        }
    }

    fn resolve(
        &self,
        base: &DirectoryBaseLease,
        operand: &PathOperand,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        if !base.confines_root()
            && let Some(node) = self.procfs_node(operand.path.as_bytes())?
        {
            if !operand.nofollow && self.procfs_namespace(operand.path.as_bytes())? {
                return Ok(Box::new(node.follow_namespace()));
            }
            if let Some(target) = node.target().filter(|_| !operand.nofollow) {
                let target = PathOperand {
                    directory: operand.directory,
                    path: GuestPathBytes::new(target).map_err(|_| RuntimePathError::Invalid)?,
                    allow_empty: false,
                    nofollow: false,
                };
                return self.resolve_projected(base, &target);
            }
            return Ok(Box::new(node));
        }
        if !base.confines_root() && operand.path.as_bytes() == b"/proc/self/exe" {
            return self.resolve_node(base, operand);
        }
        self.resolve_projected(base, operand)
    }

    fn resolve_executable(
        &self,
        base: &DirectoryBaseLease,
        path: &hl_runtime::ExecutablePath,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        self.open_executable(base, path)
    }

    fn prepare_open(
        &self,
        base: &DirectoryBaseLease,
        plan: &OpenAbiPlan,
    ) -> Result<Box<dyn PreparedPathOpen>, RuntimePathError> {
        let redirected = if base.confines_root() {
            None
        } else {
            self.procfs_plan(plan)?
        };
        let plan = redirected.as_ref().unwrap_or(plan);
        resolution::Policy::from(plan.resolve).admit()?;
        if !base.confines_root()
            && let Some(opened) = device::TerminalOpen::prepare(
                plan.operand.path.as_bytes(),
                plan.intent,
                plan.nonblocking,
                &self.terminals,
                &self.terminal_bindings,
                &self.terminal_signals,
                self.terminal_session(),
                self.tasks
                    .as_ref()
                    .zip(self.process)
                    .map(|(tasks, process)| (Arc::clone(tasks), process)),
                plan.no_controlling_terminal,
            )?
        {
            self.publish_synthetic(plan, opened.as_ref())?;
            return Ok(opened);
        }
        let synthetic_plan = Self::synthetic_plan(base, plan)?;
        let synthetic_plan = synthetic_plan.as_ref().unwrap_or(plan);
        if !base.confines_root()
            && let Some(opened) = self.synthetic_open(synthetic_plan)?
        {
            self.publish_synthetic(synthetic_plan, opened.as_ref())?;
            return Ok(opened);
        }
        if let Some(context) = self.source.projected_context() {
            return projected::Open::prepare(context, base, plan, self.projected.clone());
        }
        if !base.confines_root()
            && let Some(opened) = self.builtin(plan)?
        {
            self.publish_synthetic(plan, opened.as_ref())?;
            return Ok(opened);
        }
        let ordinary = self.ordinary()?;
        let name_binding = ordinary.name_binding(base.path(), plan.operand.path.as_bytes())?;
        if name_binding.as_ref().is_some_and(|binding| binding.read_only)
            && plan.intent.bits() & (OpenIntent::WRITE | OpenIntent::CREATE | OpenIntent::TRUNCATE | OpenIntent::APPEND)
                != 0
            && plan.intent.bits() & OpenIntent::PATH_ONLY == 0
        {
            return Err(RuntimePathError::ReadOnly);
        }
        if let Some(binding) = name_binding {
            let parent = overlay_lease::ParentLease::from(std::os::fd::OwnedFd::from(
                binding.parent.try_clone().map_err(HostError::map)?,
            ));
            let file = NativeFile::new(
                Arc::clone(&self.watches),
                binding.host.clone(),
                Arc::clone(&self.writes),
                Arc::clone(&self.ownership),
                None,
            );
            return Ok(Box::new(
                open::PendingOpen::at_guest(
                    file,
                    binding.host,
                    binding.guest,
                    plan.intent,
                    plan.mode,
                    Arc::clone(&self.paths),
                    Arc::clone(&self.terminals),
                    parent,
                    binding.leaf,
                    Arc::clone(&self.transfers),
                )
                .with_advisory_locks(self.locks.clone()),
            ));
        }
        let resolver = Resolver::new(ordinary.host(), ordinary.mounts());
        let base_path = GuestPathBytes::new(base.path().as_str().as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
        let request = ResolveRequest {
            path: &plan.operand.path,
            base: &base_path,
            nofollow_final: plan.operand.nofollow,
            no_symlinks: plan.resolve.no_symlinks,
            allow_missing_final: plan.intent.bits() & OpenIntent::CREATE != 0,
        };
        let constraints = ResolveConstraints {
            no_cross_device: plan.resolve.no_cross_device,
            no_magic_links: plan.resolve.no_magic_links,
            beneath: plan.resolve.beneath,
            in_root: plan.resolve.in_root || base.confines_root(),
        };
        let resolved = resolver
            .resolve_with(request, constraints)
            .map_err(resolution::Policy::runtime_error)?;
        let parent = resolved
            .duplicate_parent()
            .map_err(|error| resolution::Policy::runtime_error(ResolveError::Host(error)))?;
        let name = CString::new(resolved.final_name().map_or(b".".as_slice(), |name| name.as_bytes()))
            .map_err(|_| RuntimePathError::Invalid)?;
        let path = match pin::Host::path(&parent, &name) {
            Ok(path) => path,
            Err(RuntimePathError::NotFound) if plan.intent.bits() & OpenIntent::CREATE != 0 => {
                pin::Host::mutation_path(&parent, &name)?
            }
            Err(error) => return Err(error),
        };
        let guest_path = self.guest_path(&path)?;
        let guest = GuestPathBytes::new(guest_path.as_str().as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
        resolution::Policy::writable(
            ordinary.mounts(),
            ordinary.read_only(),
            ordinary.root_read_only(),
            &guest,
            plan.intent,
        )?;
        // Whatever this open creates belongs to the opening task, exactly as the mutation paths record.
        let creator = mutation::Identity::project(self).map_or((0, 0), |identity| (identity.user, identity.group));
        let file = NativeFile::new(
            Arc::clone(&self.watches),
            path.clone(),
            Arc::clone(&self.writes),
            Arc::clone(&self.ownership),
            ordinary.shm_budget(&path),
        );
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the pinned parent and terminated name remain live and status is writable.
        let exists = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                status.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0;
        if exists {
            // SAFETY: successful fstatat initialized status.
            let status = unsafe { status.assume_init() };
            if status.st_mode & libc::S_IFMT == libc::S_IFIFO {
                let key = hl_ipc::NamedFifoKey {
                    device: status.st_dev,
                    inode: status.st_ino,
                };
                return self.fifos.prepare(key, plan);
            }
        }
        if path.starts_with(ordinary.root()) {
            Ok(Box::new(
                open::PendingOpen::new(
                    file,
                    path,
                    plan.intent,
                    plan.mode,
                    ordinary.root().to_path_buf(),
                    Arc::clone(&self.paths),
                    Arc::clone(&self.terminals),
                    parent,
                    name,
                    Arc::clone(&self.transfers),
                )
                .with_creator(creator)
                .with_advisory_locks(self.locks.clone()),
            ))
        } else {
            Ok(Box::new(
                open::PendingOpen::at_guest(
                    file,
                    path,
                    guest_path,
                    plan.intent,
                    plan.mode,
                    Arc::clone(&self.paths),
                    Arc::clone(&self.terminals),
                    parent,
                    name,
                    Arc::clone(&self.transfers),
                )
                .with_creator(creator)
                .with_advisory_locks(self.locks.clone()),
            ))
        }
    }

    fn open_may_block(&self, base: &DirectoryBaseLease, plan: &OpenAbiPlan) -> Result<bool, RuntimePathError> {
        let bits = plan.intent.bits();
        if plan.nonblocking || bits & OpenIntent::READ != 0 && bits & OpenIntent::WRITE != 0 {
            return Ok(false);
        }
        // Projected opens cross an authority/provider boundary. The provider
        // is allowed to block independently of the resolved node kind.
        if self.source.is_projected() {
            return Ok(true);
        }
        Ok(self.resolve(base, &plan.operand)?.metadata()?.kind == hl_runtime::FileKind::Fifo)
    }

    fn prepare_mutation(
        &self,
        bases: &[DirectoryBaseLease],
        plan: &hl_linux::FsMutationPlan,
        identity: &AccessIdentity,
    ) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
        mutation::prepare(self, bases, plan, identity)
    }

    fn prepare_inode_link(
        &self,
        source: hl_descriptor::OperationLease,
        target_base: &DirectoryBaseLease,
        target: &PathOperand,
        identity: &AccessIdentity,
    ) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
        mutation::prepare_inode_link(self, source, target_base, target, identity)
    }

    fn prepare_descriptor_chmod(
        &self,
        source: hl_descriptor::OperationLease,
        mode: u32,
        identity: &AccessIdentity,
    ) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
        attribute::prepare_chmod(source, mode, identity)
    }

    fn prepare_descriptor_chown(
        &self,
        source: hl_descriptor::OperationLease,
        user: Option<u32>,
        group: Option<u32>,
        identity: &AccessIdentity,
    ) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
        attribute::prepare_chown(self, source, user, group, identity)
    }

    fn prepare_descriptor_times(
        &self,
        source: hl_descriptor::OperationLease,
        times: [hl_linux::TimestampChange; 2],
        identity: &AccessIdentity,
    ) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
        attribute::prepare_times(source, times, identity)
    }

    fn access_identity(&self) -> Result<AccessIdentity, RuntimePathError> {
        mutation::Identity::project(self)
    }

    fn access_identity_for(&self, effective: bool) -> Result<AccessIdentity, RuntimePathError> {
        mutation::Identity::access(self, effective)
    }
}
