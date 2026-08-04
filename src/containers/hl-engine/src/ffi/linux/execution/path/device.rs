use std::fmt;
use std::sync::Arc;

use hl_descriptor::{DescriptionIdentity, OpenFileDescription};
use hl_linux::{AccessPlan, OpenAbiPlan};
use hl_runtime::{
    BUILTIN_DEVICES, BuiltinDescription, DeviceEntropy, DeviceKind, DeviceOpenCapability, FileIdentity, FileKind,
    FileMetadata, FileTimestamp, OpenIntent, Permissions, PreparedPathOpen, ResolvedPathLease, RuntimePathError,
};

use super::super::ports::random::{EntropyError, EntropyFlags, EntropySource};
use super::NativePath;

impl NativePath {
    pub(in crate::ffi::linux::execution) fn with_entropy(mut self, entropy: Arc<dyn EntropySource>) -> Self {
        self.entropy = entropy;
        self
    }

    pub(super) fn builtin(&self, plan: &OpenAbiPlan) -> Result<Option<Box<DeviceOpen>>, RuntimePathError> {
        DeviceOpen::builtin(plan.operand.path.as_bytes(), plan.intent, Arc::clone(&self.entropy))
    }
}

pub(super) struct DeviceOpen {
    object: Arc<BuiltinDescription>,
}

impl DeviceOpen {
    pub(super) fn builtin(
        path: &[u8],
        intent: OpenIntent,
        entropy: Arc<dyn EntropySource>,
    ) -> Result<Option<Box<Self>>, RuntimePathError> {
        let kind = match path {
            b"/dev/null" => DeviceKind::Null,
            b"/dev/zero" => DeviceKind::Zero,
            b"/dev/full" => DeviceKind::Full,
            b"/dev/random" => DeviceKind::Random,
            b"/dev/urandom" => DeviceKind::Urandom,
            b"/dev/console" => DeviceKind::Terminal,
            _ => return Ok(None),
        };
        let bits = intent.bits();
        if bits & OpenIntent::DIRECTORY != 0 {
            return Err(RuntimePathError::NotDirectory);
        }
        let capability = match (bits & OpenIntent::READ != 0, bits & OpenIntent::WRITE != 0) {
            (true, false) => DeviceOpenCapability::Read,
            (false, true) => DeviceOpenCapability::Write,
            (true, true) => DeviceOpenCapability::ReadWrite,
            (false, false) if bits & OpenIntent::PATH_ONLY != 0 => DeviceOpenCapability::Path,
            (false, false) => return Err(RuntimePathError::Invalid),
        };
        let object = if matches!(kind, DeviceKind::Random | DeviceKind::Urandom) {
            BuiltinDescription::random(kind, capability, Arc::new(DeviceSource(entropy)))
        } else {
            BuiltinDescription::open(kind, capability)
        }
        .map_err(|_| RuntimePathError::Unsupported)?;
        Ok(Some(Box::new(Self {
            object: Arc::new(object),
        })))
    }

    pub(super) fn resolve(path: &[u8]) -> Option<Box<dyn ResolvedPathLease>> {
        let builtin = BUILTIN_DEVICES.iter().find(|device| device.path.as_bytes() == path)?;
        let timestamp = FileTimestamp::new(0, 0).expect("zero timestamp is valid");
        let metadata = FileMetadata {
            identity: FileIdentity {
                device: 0,
                inode: (u64::from(builtin.device.major) << 32) | u64::from(builtin.device.minor),
            },
            kind: FileKind::Character,
            permissions: Permissions::from_bits(builtin.permissions.bits()),
            links: 1,
            user: 0,
            group: 0,
            special_device: builtin.device.linux_encoded(),
            size: 0,
            blocks_512: 0,
            accessed: timestamp,
            modified: timestamp,
            changed: timestamp,
        };
        Some(Box::new(DeviceNode(metadata)))
    }
}

#[derive(Debug)]
struct DeviceNode(FileMetadata);

impl ResolvedPathLease for DeviceNode {
    fn metadata(&self) -> Result<FileMetadata, RuntimePathError> {
        Ok(self.0.clone())
    }

    fn read_link(&self) -> Result<Vec<u8>, RuntimePathError> {
        Err(RuntimePathError::Invalid)
    }

    fn access(&self, _: &AccessPlan) -> Result<(), RuntimePathError> {
        Ok(())
    }
}

struct DeviceSource(Arc<dyn EntropySource>);

impl DeviceEntropy for DeviceSource {
    fn fill(&self, output: &mut [u8]) -> Result<(), hl_descriptor::ObjectError> {
        let flags = EntropyFlags::parse(0).map_err(|_| hl_descriptor::ObjectError::InvalidArgument)?;
        let mut offset = 0;
        while offset < output.len() {
            match self.0.draw(&mut output[offset..], flags) {
                Ok(0) | Err(EntropyError::WouldBlock) => return Err(hl_descriptor::ObjectError::WouldBlock),
                Ok(count) if count <= output.len() - offset => offset += count,
                Ok(_) | Err(EntropyError::Failed) => return Err(hl_descriptor::ObjectError::Io),
                Err(EntropyError::Interrupted) => continue,
            }
        }
        Ok(())
    }
}

impl fmt::Debug for DeviceOpen {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DeviceOpen")
    }
}

impl PreparedPathOpen for DeviceOpen {
    fn object(&self) -> Arc<dyn OpenFileDescription> {
        self.object.clone()
    }

    fn commit(&mut self) -> Result<(), RuntimePathError> {
        Ok(())
    }

    fn rollback(self: Box<Self>) {}
}

pub(super) struct DetachedSignals;

impl hl_runtime::TerminalSignalSink for DetachedSignals {
    fn publish(
        &self,
        _: Option<hl_descriptor::OperationActor>,
        _: hl_runtime::TerminalId,
        _: Option<hl_runtime::TerminalForegroundGroup>,
        _: hl_runtime::TerminalSignal,
    ) {
    }
}

pub(super) struct TerminalOpen {
    object: Arc<hl_runtime::TerminalDescription>,
    bindings: Arc<hl_runtime::TerminalBindings>,
    acquisition: Option<(
        Arc<hl_runtime::TerminalCatalog>,
        hl_task::SessionId,
        hl_runtime::TerminalId,
        Option<(Arc<hl_task::TaskRegistry>, hl_task::ProcessId)>,
    )>,
}

impl TerminalOpen {
    pub(super) fn prepare(
        path: &[u8],
        intent: OpenIntent,
        nonblocking: bool,
        catalog: &Arc<hl_runtime::TerminalCatalog>,
        bindings: &Arc<hl_runtime::TerminalBindings>,
        signals: &Arc<dyn hl_runtime::TerminalSignalSink>,
        session: Option<(hl_task::SessionId, bool, bool)>,
        task: Option<(Arc<hl_task::TaskRegistry>, hl_task::ProcessId)>,
        no_controlling_terminal: bool,
    ) -> Result<Option<Box<Self>>, RuntimePathError> {
        let (pair, endpoint, acquisition) = if path == b"/dev/tty" {
            let (session, _, true) = session.ok_or(RuntimePathError::NoDevice)? else {
                return Err(RuntimePathError::NoDevice);
            };
            (
                catalog
                    .controlling(session.number())
                    .map_err(|_| RuntimePathError::NoDevice)?,
                hl_runtime::TerminalEndpoint::Slave,
                None,
            )
        } else if matches!(path, b"/dev/ptmx" | b"/dev/pts/ptmx") {
            (
                catalog.allocate().map_err(|_| RuntimePathError::Io)?,
                hl_runtime::TerminalEndpoint::Master,
                None,
            )
        } else if let Some(index) = Self::devpts_index(path) {
            let pair = catalog.current(index).map_err(|_| RuntimePathError::NotFound)?;
            let acquisition = session
                .filter(|(_, leader, _)| *leader && !no_controlling_terminal)
                .map(|(session, _, _)| (Arc::clone(catalog), session, pair.id(), task));
            (pair, hl_runtime::TerminalEndpoint::Slave, acquisition)
        } else {
            return Ok(None);
        };
        let bits = intent.bits();
        if bits & (OpenIntent::DIRECTORY | OpenIntent::PATH_ONLY) != 0 {
            return Err(RuntimePathError::Invalid);
        }
        let status = hl_descriptor::StatusFlags::from_bits(if nonblocking {
            hl_descriptor::StatusFlags::NONBLOCKING
        } else {
            0
        });
        let object = Arc::new(hl_runtime::TerminalDescription::with_status(
            pair,
            endpoint,
            Arc::downgrade(catalog),
            Arc::clone(signals),
            status,
        ));
        Ok(Some(Box::new(Self {
            object,
            bindings: Arc::clone(bindings),
            acquisition,
        })))
    }

    pub(super) fn terminal_node(
        path: &[u8],
        catalog: &hl_runtime::TerminalCatalog,
    ) -> Option<Box<dyn ResolvedPathLease>> {
        let (major, minor, permissions) = if matches!(path, b"/dev/ptmx" | b"/dev/pts/ptmx") {
            (5_u32, 2_u32, 0o666)
        } else {
            let index = Self::devpts_index(path)?;
            catalog.current(index).ok()?;
            (136, u32::from(index), 0o620)
        };
        let timestamp = FileTimestamp::new(0, 0).expect("zero timestamp is valid");
        let device = hl_runtime::DeviceId::new(major, minor);
        Some(Box::new(DeviceNode(FileMetadata {
            identity: FileIdentity {
                device: 0,
                inode: (u64::from(major) << 32) | u64::from(minor),
            },
            kind: FileKind::Character,
            permissions: Permissions::from_bits(permissions),
            links: 1,
            user: 0,
            group: 0,
            special_device: device.linux_encoded(),
            size: 0,
            blocks_512: 0,
            accessed: timestamp,
            modified: timestamp,
            changed: timestamp,
        })))
    }

    fn devpts_index(path: &[u8]) -> Option<u16> {
        let digits = path.strip_prefix(b"/dev/pts/")?;
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        digits.iter().try_fold(0_u16, |value, digit| {
            value.checked_mul(10)?.checked_add(u16::from(*digit - b'0'))
        })
    }
}

impl fmt::Debug for TerminalOpen {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TerminalOpen")
    }
}

impl PreparedPathOpen for TerminalOpen {
    fn object(&self) -> Arc<dyn OpenFileDescription> {
        self.object.clone()
    }

    fn bind(&mut self, identity: DescriptionIdentity) -> Result<(), RuntimePathError> {
        self.object.bind(identity, &self.bindings);
        Ok(())
    }

    fn commit(&mut self) -> Result<(), RuntimePathError> {
        if let Some((catalog, session, pair, task)) = self.acquisition.take()
            && let Ok(created) = catalog.acquire_changed(session.number(), pair)
            && let Some((tasks, process)) = task
            && tasks.attach_terminal(process, session).is_err()
            && created
        {
            let _ = catalog.detach(session.number(), pair);
        }
        Ok(())
    }

    fn rollback(self: Box<Self>) {
        self.object.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_bypass() {
        let catalog = Arc::new(hl_runtime::TerminalCatalog::default());
        let bindings = Arc::new(hl_runtime::TerminalBindings::default());
        let signals: Arc<dyn hl_runtime::TerminalSignalSink> = Arc::new(DetachedSignals);
        let intent = OpenIntent::from_bits(OpenIntent::READ | OpenIntent::DIRECTORY);
        assert!(matches!(
            TerminalOpen::prepare(
                b"/dev/pts",
                intent,
                false,
                &catalog,
                &bindings,
                &signals,
                None,
                None,
                false
            ),
            Ok(None),
        ));
    }

    #[test]
    fn slave_open_reacquires_terminal_after_process_detach() {
        let catalog = Arc::new(hl_runtime::TerminalCatalog::default());
        let pair = catalog.allocate().unwrap();
        let bindings = Arc::new(hl_runtime::TerminalBindings::default());
        let signals: Arc<dyn hl_runtime::TerminalSignalSink> = Arc::new(DetachedSignals);
        let tasks = Arc::new(hl_task::TaskRegistry::new(hl_task::RegistryConfig::default()).unwrap());
        let credentials = hl_task::ProcessCredentials::new(1000, 1000, &[], 8).unwrap();
        let (process, _) = tasks.create_init(credentials, hl_task::ProcessLimits::empty()).unwrap();
        let session = tasks.session_id(process).unwrap();
        tasks
            .prepare_terminal_transition(process, hl_task::TerminalTransition::Detach)
            .unwrap()
            .commit();
        assert_eq!(tasks.terminal_session(process).unwrap(), None);
        assert!(matches!(
            TerminalOpen::prepare(
                b"/dev/tty",
                OpenIntent::from_bits(OpenIntent::READ),
                false,
                &catalog,
                &bindings,
                &signals,
                Some((session, true, false)),
                Some((Arc::clone(&tasks), process)),
                false,
            ),
            Err(RuntimePathError::NoDevice),
        ));

        let mut opened = TerminalOpen::prepare(
            b"/dev/pts/0",
            OpenIntent::from_bits(OpenIntent::READ | OpenIntent::WRITE),
            false,
            &catalog,
            &bindings,
            &signals,
            Some((session, true, false)),
            Some((Arc::clone(&tasks), process)),
            false,
        )
        .unwrap()
        .unwrap();
        opened.commit().unwrap();

        assert_eq!(tasks.terminal_session(process).unwrap(), Some(session));
        assert_eq!(catalog.controlling(session.number()).unwrap().id(), pair.id());
    }

    #[test]
    fn failed_task_attachment_compensates_new_catalog_binding() {
        let catalog = Arc::new(hl_runtime::TerminalCatalog::default());
        catalog.allocate().unwrap();
        let bindings = Arc::new(hl_runtime::TerminalBindings::default());
        let signals: Arc<dyn hl_runtime::TerminalSignalSink> = Arc::new(DetachedSignals);
        let tasks = Arc::new(hl_task::TaskRegistry::new(hl_task::RegistryConfig::default()).unwrap());
        let credentials = hl_task::ProcessCredentials::new(1000, 1000, &[], 8).unwrap();
        let (leader, leader_thread) = tasks.create_init(credentials, hl_task::ProcessLimits::empty()).unwrap();
        let session = tasks.session_id(leader).unwrap();
        let child_plan = tasks.begin_fork_process(leader_thread).unwrap();
        let child = child_plan.process();
        tasks.commit_fork_process(child_plan).unwrap();
        tasks.create_session(child).unwrap();
        let mut opened = TerminalOpen::prepare(
            b"/dev/pts/0",
            OpenIntent::from_bits(OpenIntent::READ | OpenIntent::WRITE),
            false,
            &catalog,
            &bindings,
            &signals,
            Some((session, true, false)),
            Some((Arc::clone(&tasks), child)),
            false,
        )
        .unwrap()
        .unwrap();

        opened.commit().unwrap();

        assert!(catalog.controlling(session.number()).is_err());
        assert_eq!(tasks.terminal_session(child).unwrap(), None);
    }
}
