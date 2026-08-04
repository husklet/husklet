use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::{GuestPath, HostError, NativePath, metadata, pin, resolution};
use hl_linux::AccessPlan;
use hl_runtime::{
    DirectoryBaseLease, ExecutablePath, FileMetadata, GuestPathBytes, OpenIntent, ResolveError, ResolveRequest,
    ResolvedPathLease, Resolver, RuntimeExecError, RuntimePathError,
};

#[derive(Debug)]
pub(super) struct ProcessExecutable(pub(super) Vec<u8>);

impl ResolvedPathLease for ProcessExecutable {
    fn metadata(&self) -> Result<FileMetadata, RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }
    fn read_link(&self) -> Result<Vec<u8>, RuntimePathError> {
        Ok(self.0.clone())
    }
    fn access(&self, _: &AccessPlan) -> Result<(), RuntimePathError> {
        Err(RuntimePathError::Access)
    }
}

pub(in crate::ffi::linux::execution) struct ExecTarget {
    pub(in crate::ffi::linux::execution) identity: Arc<Mutex<Vec<u8>>>,
    pub(in crate::ffi::linux::execution) executable: Vec<u8>,
    pub(in crate::ffi::linux::execution) plan: hl_linux::ExecPlan,
    pub(in crate::ffi::linux::execution) execfn: Vec<u8>,
}

impl NativePath {
    pub(in crate::ffi::linux::execution) fn stage_projected(
        &self,
        plan: &hl_linux::ExecPlan,
    ) -> Result<ExecTarget, RuntimeExecError> {
        let logical = self.executable.lock().map_err(|_| RuntimeExecError::Failed)?.clone();
        if plan.path != logical && plan.path != b"/proc/self/exe" {
            return Err(RuntimeExecError::NotFound);
        }
        let mut resolved = plan.clone();
        resolved.path = logical.clone();
        Ok(ExecTarget {
            identity: Arc::clone(&self.executable),
            executable: logical,
            plan: resolved,
            execfn: plan.path.clone(),
        })
    }

    pub(super) fn open_executable(
        &self,
        base: &DirectoryBaseLease,
        request: &ExecutablePath,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        let ordinary = self.ordinary()?;
        let resolver = Resolver::new(ordinary.host(), ordinary.mounts());
        let base_path = GuestPathBytes::new(base.path().as_str().as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
        let resolved = resolver
            .resolve_with(
                ResolveRequest {
                    path: &request.path,
                    base: &base_path,
                    nofollow_final: request.nofollow,
                    no_symlinks: false,
                    allow_missing_final: false,
                },
                base.resolve_constraints(),
            )
            .map_err(resolution::Policy::runtime_error)?;
        let parent = resolved
            .duplicate_parent()
            .map_err(|error| resolution::Policy::runtime_error(ResolveError::Host(error)))?;
        let name = CString::new(resolved.final_name().map_or(b".".as_slice(), |name| name.as_bytes()))
            .map_err(|_| RuntimePathError::Invalid)?;
        let intent = OpenIntent::READ | if request.nofollow { OpenIntent::NOFOLLOW } else { 0 };
        let file = pin::Host::open(&parent, &name, OpenIntent::from_bits(intent), 0)?;
        let metadata = file.metadata().map_err(HostError::map)?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(RuntimePathError::Access);
        }
        if self
            .writes
            .lock()
            .map_err(|_| RuntimePathError::Io)?
            .get(&(metadata.dev(), metadata.ino()))
            .is_some_and(|count| *count != 0)
        {
            return Err(RuntimePathError::TextBusy);
        }
        Ok(Box::new(OpenedImage { file: Mutex::new(file) }))
    }

    pub(in crate::ffi::linux::execution) fn descriptor_exec(
        &self,
        metadata: &hl_descriptor::OfdMetadata,
    ) -> Result<Vec<u8>, RuntimeExecError> {
        self.paths
            .lock()
            .map_err(|_| RuntimeExecError::Failed)?
            .get(&(metadata.device, metadata.inode))
            .map(|opened| opened.guest.as_str().as_bytes().to_vec())
            .ok_or(RuntimeExecError::BadDescriptor)
    }

    pub(in crate::ffi::linux::execution) fn stage_exec(
        &self,
        plan: &hl_linux::ExecPlan,
        working: &GuestPath,
    ) -> Result<ExecTarget, RuntimeExecError> {
        const RECURSION_LIMIT: usize = 4;
        let execfn = plan.path.clone();
        let mut candidate = plan.clone();
        let mut identity = None;
        for depth in 0..=RECURSION_LIMIT {
            let (resolved, guest) = self.exec_path(&candidate, working)?;
            if identity.is_none() {
                identity = Some(guest.clone());
            }
            let Some((interpreter, argument)) = Self::script_line(&resolved)? else {
                candidate.path = guest;
                return Ok(ExecTarget {
                    identity: Arc::clone(&self.executable),
                    executable: identity.ok_or(RuntimeExecError::Failed)?,
                    plan: candidate,
                    execfn,
                });
            };
            if depth == RECURSION_LIMIT {
                return Err(RuntimeExecError::Loop);
            }
            let mut arguments = Vec::with_capacity(candidate.arguments.len() + 2);
            arguments.push(interpreter.clone());
            if let Some(argument) = argument {
                arguments.push(argument);
            }
            arguments.push(candidate.path.clone());
            arguments.extend(candidate.arguments.iter().skip(1).cloned());
            candidate = hl_linux::ExecPlan {
                directory: Some(-100),
                path: interpreter,
                arguments,
                environment: candidate.environment,
                flags: 0,
            };
        }
        Err(RuntimeExecError::Loop)
    }

    fn exec_path(
        &self,
        plan: &hl_linux::ExecPlan,
        working: &GuestPath,
    ) -> Result<(PathBuf, Vec<u8>), RuntimeExecError> {
        if plan.path.is_empty() || plan.directory.is_some_and(|value| value != -100) {
            return Err(RuntimeExecError::Unsupported);
        }
        let projected = if plan.path == b"/proc/self/exe" {
            self.executable.lock().map_err(|_| RuntimeExecError::Failed)?.clone()
        } else {
            plan.path.clone()
        };
        let ordinary = self.ordinary().map_err(Self::exec_error)?;
        let resolver = Resolver::new(ordinary.host(), ordinary.mounts());
        let base = GuestPathBytes::new(working.as_str().as_bytes()).map_err(|_| RuntimeExecError::Invalid)?;
        let path = GuestPathBytes::new(&projected).map_err(|_| RuntimeExecError::Invalid)?;
        let target = resolver
            .resolve(ResolveRequest {
                path: &path,
                base: &base,
                nofollow_final: plan.flags & 0x100 != 0,
                no_symlinks: false,
                allow_missing_final: false,
            })
            .map_err(resolution::Policy::runtime_error)
            .map_err(Self::exec_error)?;
        let parent = target
            .duplicate_parent()
            .map_err(|error| resolution::Policy::runtime_error(ResolveError::Host(error)))
            .map_err(Self::exec_error)?;
        let name = CString::new(target.final_name().map_or(b".".as_slice(), |name| name.as_bytes()))
            .map_err(|_| RuntimeExecError::Invalid)?;
        let resolved = pin::Host::path(&parent, &name).map_err(Self::exec_error)?;
        if plan.flags & 0x100 != 0
            && std::fs::symlink_metadata(&resolved)
                .map_err(HostError::map)
                .map_err(Self::exec_error)?
                .file_type()
                .is_symlink()
        {
            return Err(RuntimeExecError::Loop);
        }
        let guest = self.guest_path(&resolved).map_err(Self::exec_error)?;
        let metadata = resolved.metadata().map_err(HostError::map).map_err(Self::exec_error)?;
        let identity = (metadata.dev(), metadata.ino());
        if self
            .writes
            .lock()
            .map_err(|_| RuntimeExecError::Failed)?
            .get(&identity)
            .is_some_and(|count| *count != 0)
        {
            return Err(RuntimeExecError::TextBusy);
        }
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(RuntimeExecError::Access);
        }
        Ok((resolved, guest.as_str().as_bytes().to_vec()))
    }

    fn script_line(path: &Path) -> Result<Option<(Vec<u8>, Option<Vec<u8>>)>, RuntimeExecError> {
        const HEADER_SIZE: u64 = 256;
        let mut bytes = Vec::with_capacity(HEADER_SIZE as usize + 1);
        File::open(path)
            .map_err(HostError::map)
            .map_err(Self::exec_error)?
            .take(HEADER_SIZE + 1)
            .read_to_end(&mut bytes)
            .map_err(HostError::map)
            .map_err(Self::exec_error)?;
        if !bytes.starts_with(b"#!") {
            return Ok(None);
        }
        let end = bytes
            .iter()
            .take(HEADER_SIZE as usize)
            .position(|byte| *byte == b'\n')
            .or_else(|| (bytes.len() <= HEADER_SIZE as usize).then_some(bytes.len()))
            .ok_or(RuntimeExecError::Format)?;
        let mut line = &bytes[2..end];
        while line.last().is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\r')) {
            line = &line[..line.len() - 1];
        }
        while line.first().is_some_and(|byte| matches!(byte, b' ' | b'\t')) {
            line = &line[1..];
        }
        if line.is_empty() || line.contains(&0) {
            return Err(RuntimeExecError::Format);
        }
        let split = line
            .iter()
            .position(|byte| matches!(byte, b' ' | b'\t'))
            .unwrap_or(line.len());
        let interpreter = line[..split].to_vec();
        let mut rest = &line[split..];
        while rest.first().is_some_and(|byte| matches!(byte, b' ' | b'\t')) {
            rest = &rest[1..];
        }
        Ok(Some((interpreter, (!rest.is_empty()).then(|| rest.to_vec()))))
    }

    const fn exec_error(error: RuntimePathError) -> RuntimeExecError {
        match error {
            RuntimePathError::NotFound => RuntimeExecError::NotFound,
            RuntimePathError::Access | RuntimePathError::ReadOnly => RuntimeExecError::Access,
            RuntimePathError::Loop => RuntimeExecError::Loop,
            RuntimePathError::Invalid => RuntimeExecError::Invalid,
            RuntimePathError::NameTooLong => RuntimeExecError::NameTooLong,
            RuntimePathError::TooLarge => RuntimeExecError::TooBig,
            RuntimePathError::NoSpace => RuntimeExecError::TooBig,
            RuntimePathError::BadDescriptor => RuntimeExecError::BadDescriptor,
            RuntimePathError::Unsupported => RuntimeExecError::Unsupported,
            RuntimePathError::TextBusy => RuntimeExecError::TextBusy,
            _ => RuntimeExecError::Failed,
        }
    }
}

#[derive(Debug)]
struct OpenedImage {
    file: Mutex<File>,
}

impl ResolvedPathLease for OpenedImage {
    fn metadata(&self) -> Result<FileMetadata, RuntimePathError> {
        metadata::HostMetadata::file(&self.file.lock().unwrap_or_else(|error| error.into_inner()))
    }

    fn read_link(&self) -> Result<Vec<u8>, RuntimePathError> {
        Err(RuntimePathError::Invalid)
    }

    fn access(&self, _: &AccessPlan) -> Result<(), RuntimePathError> {
        Ok(())
    }

    fn read_image(&self, maximum: usize) -> Result<Vec<u8>, RuntimePathError> {
        let file = self.file.lock().unwrap_or_else(|error| error.into_inner());
        let metadata = file.metadata().map_err(HostError::map)?;
        if metadata.len() > maximum as u64 {
            return Err(RuntimePathError::TooLarge);
        }
        let mut reader = file.try_clone().map_err(HostError::map)?;
        reader.seek(SeekFrom::Start(0)).map_err(HostError::map)?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        let limit = u64::try_from(maximum)
            .map_err(|_| RuntimePathError::TooLarge)?
            .saturating_add(1);
        reader.take(limit).read_to_end(&mut bytes).map_err(HostError::map)?;
        if bytes.len() > maximum {
            Err(RuntimePathError::TooLarge)
        } else {
            Ok(bytes)
        }
    }

    fn executable_access(&self, _: &ExecutablePath) -> Result<(), RuntimePathError> {
        Ok(())
    }
}
