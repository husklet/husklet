use std::sync::Arc;

use hl_descriptor::{DescriptorError, DescriptorTable};
use hl_linux::ExecPlan;
use hl_loader::{ImageRole, ImageSource, ImageSourceError};
use hl_task::ProcessId;
use hl_vfs::{FileKind, GuestPathBytes, OpenDirectory, PathError};

use crate::{
    DescriptorImageSlot, ExecutablePath, ResolvedPathLease, RuntimeExecError, RuntimePathError, RuntimePathHost,
    SourceFactory,
};

const AT_FDCWD: i32 = -100;
const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
const AT_EMPTY_PATH: u32 = 0x1000;

pub struct VfsSourceFactory {
    host: Arc<dyn RuntimePathHost>,
    descriptors: Arc<dyn CurrentDescriptorTable>,
}

pub trait CurrentDescriptorTable: Send + Sync {
    fn current_descriptor_table(&self) -> Arc<DescriptorTable>;
}

impl CurrentDescriptorTable for DescriptorImageSlot {
    fn current_descriptor_table(&self) -> Arc<DescriptorTable> {
        self.current().1
    }
}

impl VfsSourceFactory {
    #[must_use]
    pub fn new(host: Arc<dyn RuntimePathHost>, descriptors: Arc<dyn CurrentDescriptorTable>) -> Self {
        Self { host, descriptors }
    }

    fn operand(plan: &ExecPlan) -> Result<ExecutablePath, RuntimeExecError> {
        let path = GuestPathBytes::new(&plan.path).map_err(Self::path_value)?;
        Ok(ExecutablePath {
            path,
            nofollow: plan.flags & AT_SYMLINK_NOFOLLOW != 0,
        })
    }

    const fn path_value(error: PathError) -> RuntimeExecError {
        match error {
            PathError::TooLong => RuntimeExecError::NameTooLong,
            PathError::ContainsNul => RuntimeExecError::Invalid,
            PathError::Empty | PathError::TooManyComponents | PathError::InvalidComponent => RuntimeExecError::Invalid,
        }
    }

    fn resolve(
        &self,
        plan: &ExecPlan,
        operand: &ExecutablePath,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimeExecError> {
        if plan.path.is_empty() {
            return self.resolve_empty(plan);
        }
        let directory = OpenDirectory::from_raw(plan.directory.unwrap_or(AT_FDCWD) as i64 as u64);
        let base = if operand.path.is_absolute() || directory.raw() == AT_FDCWD as i64 as u64 {
            self.host.root_base().map_err(Self::path)?
        } else {
            let descriptors = self.descriptors.current_descriptor_table();
            let lease = descriptors.pin(directory.raw() as i32).map_err(Self::descriptor)?;
            self.host.descriptor_base(lease).map_err(Self::path)?
        };
        self.host.resolve_executable(&base, operand).map_err(Self::path)
    }

    fn resolve_empty(&self, plan: &ExecPlan) -> Result<Box<dyn ResolvedPathLease>, RuntimeExecError> {
        if plan.flags & AT_EMPTY_PATH == 0 {
            return Err(RuntimeExecError::NotFound);
        }
        let descriptor = plan.directory.ok_or(RuntimeExecError::BadDescriptor)?;
        let descriptors = self.descriptors.current_descriptor_table();
        let lease = descriptors.pin(descriptor).map_err(Self::descriptor)?;
        self.host.descriptor_node(lease).map_err(Self::path)
    }

    fn validate(operand: &ExecutablePath, node: &dyn ResolvedPathLease) -> Result<(), RuntimeExecError> {
        let metadata = node.metadata().map_err(Self::path)?;
        if operand.nofollow && metadata.kind == FileKind::Symlink {
            return Err(RuntimeExecError::Loop);
        }
        if metadata.kind != FileKind::Regular {
            return Err(RuntimeExecError::Access);
        }
        node.executable_access(operand).map_err(Self::path)?;
        Ok(())
    }

    const fn descriptor(error: DescriptorError) -> RuntimeExecError {
        match error {
            DescriptorError::BadDescriptor => RuntimeExecError::BadDescriptor,
            _ => RuntimeExecError::Failed,
        }
    }

    const fn path(error: RuntimePathError) -> RuntimeExecError {
        match error {
            RuntimePathError::BadDescriptor => RuntimeExecError::BadDescriptor,
            RuntimePathError::NotFound => RuntimeExecError::NotFound,
            RuntimePathError::NoDevice => RuntimeExecError::Failed,
            RuntimePathError::Access | RuntimePathError::ReadOnly => RuntimeExecError::Access,
            RuntimePathError::Loop => RuntimeExecError::Loop,
            RuntimePathError::TooLarge | RuntimePathError::FileTooLarge => RuntimeExecError::TooBig,
            RuntimePathError::NameTooLong => RuntimeExecError::NameTooLong,
            RuntimePathError::Invalid => RuntimeExecError::Invalid,
            RuntimePathError::TextBusy => RuntimeExecError::TextBusy,
            RuntimePathError::NotDirectory
            | RuntimePathError::IsDirectory
            | RuntimePathError::OperationNotPermitted
            | RuntimePathError::DirectoryNotEmpty
            | RuntimePathError::Exists
            | RuntimePathError::CrossDevice
            | RuntimePathError::Unsupported
            | RuntimePathError::NoSpace
            | RuntimePathError::Quota
            | RuntimePathError::WouldBlock
            | RuntimePathError::Io => RuntimeExecError::Failed,
        }
    }
}

impl SourceFactory for VfsSourceFactory {
    type Source = VfsImageSource;

    fn open(&self, _: ProcessId, plan: &ExecPlan) -> Result<Self::Source, RuntimeExecError> {
        let operand = Self::operand(plan)?;
        let main = self.resolve(plan, &operand)?;
        Self::validate(&operand, main.as_ref())?;
        Ok(VfsImageSource {
            host: Arc::clone(&self.host),
            main,
        })
    }
}

pub struct VfsImageSource {
    host: Arc<dyn RuntimePathHost>,
    main: Box<dyn ResolvedPathLease>,
}

impl VfsImageSource {
    fn interpreter(&self, path: &[u8]) -> Result<Box<dyn ResolvedPathLease>, ImageSourceError> {
        let operand = ExecutablePath {
            path: GuestPathBytes::new(path).map_err(|error| match error {
                PathError::TooLong => ImageSourceError::TooLarge,
                _ => ImageSourceError::NotFound,
            })?,
            nofollow: false,
        };
        let base = self.host.root_base().map_err(Self::source_error)?;
        let node = self
            .host
            .resolve_executable(&base, &operand)
            .map_err(Self::source_error)?;
        let metadata = node.metadata().map_err(Self::source_error)?;
        if metadata.kind != FileKind::Regular {
            return Err(ImageSourceError::AccessDenied);
        }
        node.executable_access(&operand).map_err(Self::source_error)?;
        Ok(node)
    }

    const fn source_error(error: RuntimePathError) -> ImageSourceError {
        match error {
            RuntimePathError::NotFound => ImageSourceError::NotFound,
            RuntimePathError::Access | RuntimePathError::ReadOnly => ImageSourceError::AccessDenied,
            RuntimePathError::TooLarge => ImageSourceError::TooLarge,
            RuntimePathError::NoSpace => ImageSourceError::TooLarge,
            RuntimePathError::NameTooLong => ImageSourceError::TooLarge,
            RuntimePathError::TextBusy => ImageSourceError::AccessDenied,
            _ => ImageSourceError::Io,
        }
    }
}

impl ImageSource for VfsImageSource {
    fn read_image(&mut self, role: ImageRole, path: &[u8], maximum: usize) -> Result<Vec<u8>, ImageSourceError> {
        match role {
            ImageRole::Main => self.main.read_image(maximum).map_err(Self::source_error),
            ImageRole::Interpreter => self.interpreter(path)?.read_image(maximum).map_err(Self::source_error),
        }
    }
}
