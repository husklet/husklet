use std::path::{Path, PathBuf};

use hl_linux::PathOperand;
use hl_runtime::{
    DirectoryBaseLease, FilesystemStats, GuestPath, ResolvedMetadata, ResolvedPathLease, RuntimePathError,
    RuntimeXattrMutation, XattrName,
};

use super::{NativePath, device, executable::ProcessExecutable, metadata, pin};

#[derive(Debug)]
struct ReadOnlyNode(Box<dyn ResolvedPathLease>);

impl ResolvedPathLease for ReadOnlyNode {
    fn metadata(&self) -> Result<hl_runtime::FileMetadata, RuntimePathError> {
        self.0.metadata()
    }

    fn resolved_metadata(&self) -> Result<ResolvedMetadata, RuntimePathError> {
        self.0.resolved_metadata()
    }

    fn filesystem(&self) -> Result<FilesystemStats, RuntimePathError> {
        self.0.filesystem()
    }

    fn truncate(&self, _size: u64) -> Result<(), RuntimePathError> {
        Err(RuntimePathError::ReadOnly)
    }

    fn read_link(&self) -> Result<Vec<u8>, RuntimePathError> {
        self.0.read_link()
    }

    fn access(&self, plan: &hl_linux::AccessPlan) -> Result<(), RuntimePathError> {
        if plan.access.bits() & 2 != 0 {
            Err(RuntimePathError::ReadOnly)
        } else {
            self.0.access(plan)
        }
    }

    fn xattr_get(&self, name: &XattrName) -> Result<Vec<u8>, hl_linux::Errno> {
        self.0.xattr_get(name)
    }

    fn xattr_list(&self) -> Result<Vec<u8>, hl_linux::Errno> {
        self.0.xattr_list()
    }

    fn prepare_xattr(
        &self,
        _mutation: RuntimeXattrMutation,
    ) -> Result<Box<dyn hl_runtime::PreparedXattrMutation>, hl_linux::Errno> {
        Err(hl_linux::Errno::EROFS)
    }
}

impl NativePath {
    pub(super) fn resolve_node(
        &self,
        base: &DirectoryBaseLease,
        operand: &PathOperand,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        if !base.confines_root() {
            if let Some(node) = device::TerminalOpen::terminal_node(operand.path.as_bytes(), &self.terminals) {
                return Ok(node);
            }
            if let Some(node) = device::DeviceOpen::resolve(operand.path.as_bytes()) {
                return Ok(node);
            }
        }
        if !base.confines_root() && operand.path.as_bytes() == b"/proc/self/exe" {
            let target = self
                .executable
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if target.is_empty() {
                return Err(RuntimePathError::NotFound);
            }
            return Ok(Box::new(ProcessExecutable(target)));
        }
        let ordinary = self.ordinary()?;
        if let Some(binding) = ordinary.name_binding(base.path(), operand.path.as_bytes())? {
            return Ok(Box::new(ReadOnlyNode(Box::new(metadata::Node::new(
                binding.host,
                binding.guest,
                std::sync::Arc::clone(&self.ownership),
            )))));
        }
        let (parent, name) = self.resolve_anchor(base, operand)?;
        Ok(Box::new(metadata::Node::anchored(
            parent,
            name,
            self.source.native_arc()?,
            std::sync::Arc::clone(&self.ownership),
        )))
    }

    pub(super) fn resolve_path(
        &self,
        base: &DirectoryBaseLease,
        operand: &PathOperand,
    ) -> Result<PathBuf, RuntimePathError> {
        let ordinary = self.ordinary()?;
        if let Some(binding) = ordinary.name_binding(base.path(), operand.path.as_bytes())? {
            return Ok(binding.host);
        }
        let (parent, name) = self.resolve_anchor(base, operand)?;
        pin::Host::path(&parent, &name)
    }

    /// Resolves to the pinned parent and leaf name the walk already produced,
    /// leaving the host path unrendered.
    fn resolve_anchor(
        &self,
        base: &DirectoryBaseLease,
        operand: &PathOperand,
    ) -> Result<(super::overlay_lease::ParentLease, std::ffi::CString), RuntimePathError> {
        let ordinary = self.ordinary()?;
        let resolver = hl_runtime::Resolver::new(ordinary.host(), ordinary.mounts());
        let base_path =
            hl_runtime::GuestPathBytes::new(base.path().as_str().as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
        let resolved = resolver
            .resolve_with(
                hl_runtime::ResolveRequest {
                    path: &operand.path,
                    base: &base_path,
                    nofollow_final: operand.nofollow,
                    no_symlinks: false,
                    allow_missing_final: false,
                },
                base.resolve_constraints(),
            )
            .map_err(super::resolution::Policy::runtime_error)?;
        let parent = resolved
            .duplicate_parent()
            .map_err(|error| super::resolution::Policy::runtime_error(hl_runtime::ResolveError::Host(error)))?;
        let name = resolved.final_name().map_or_else(
            || std::ffi::CString::new(".").expect("dot has no NUL"),
            |name| std::ffi::CString::new(name.as_bytes()).expect("validated guest component has no NUL"),
        );
        Ok((parent, name))
    }

    pub(super) fn guest_path(&self, path: &Path) -> Result<GuestPath, RuntimePathError> {
        self.ordinary()?.guest_path(path)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use hl_linux::PathOperand;
    use hl_runtime::{GuestPathBytes, OpenDirectory, RuntimePathHost};

    use super::NativePath;

    static NEXT: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn root_resolves_to_node() {
        let path = std::env::temp_dir().join(format!(
            "hl-route-root-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&path).unwrap();
        let bytes = path.as_os_str().as_encoded_bytes();
        let host = NativePath::new(bytes, super::super::watch::Hub::new(bytes).unwrap()).unwrap();
        let operand = PathOperand {
            directory: OpenDirectory::default(),
            path: GuestPathBytes::new(b"/").unwrap(),
            allow_empty: false,
            nofollow: true,
        };
        let node = host.resolve(&host.root_base().unwrap(), &operand).unwrap();
        assert!(node.metadata().is_ok());
        drop(node);
        drop(host);
        std::fs::remove_dir_all(path).unwrap();
    }
}
