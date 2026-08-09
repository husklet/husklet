//! Typed process-filesystem projection over domain-owned snapshots.

mod cgroup;
mod file;
mod metadata;
mod model;
mod mount;
mod open;
mod query;
mod source;
mod stat;

pub use cgroup::View as CgroupView;
pub use model::{
    AddressSpaceView, CpuModel, CpuTicks, CpuView, DescriptorView, InternetSocketView, LimitResource, LimitView,
    MemoryRegionLabel, MemoryRegionView, MemoryView, NetworkInterfaceView, NetworkView, NodeKind, ProcessIdentity,
    ProcessState, ProcessView, SystemView, ThreadIdentity, UnixSocketView, UtsView,
};
pub use mount::{MountEntry, MountView};
pub use source::Source;
pub use stat::{Error as StatError, Input as StatInput, State as StatState, View as StatView};

use std::sync::Arc;

use hl_descriptor::OfdMetadata;

const PROCESS_IO: &[u8] =
    b"rchar: 0\nwchar: 0\nsyscr: 0\nsyscw: 0\nread_bytes: 0\nwrite_bytes: 0\ncancelled_write_bytes: 0\n";
const FILESYSTEMS: &[u8] = b"nodev\tsysfs\nnodev\ttmpfs\nnodev\tproc\nnodev\tdevtmpfs\nnodev\tdevpts\nnodev\tmqueue\nnodev\tcgroup2\nnodev\toverlay\n\text3\n\text2\n\text4\n";
const KERNEL_COMMAND_LINE: &[u8] = b"root=/dev/sda1 ro quiet\n";
const DEVICES: &[u8] = b"Character devices:\n  1 mem\n  5 /dev/tty\n  5 /dev/console\n  5 /dev/ptmx\n136 pts\n\nBlock devices:\n  7 loop\n  8 sd\n 253 device-mapper\n 259 blkext\n";
const TTY_DRIVERS: &[u8] = b"/dev/tty             /dev/tty        5       0 system:/dev/tty\n/dev/console         /dev/console    5       1 system:console\n/dev/ptmx            /dev/ptmx       5       2 system\nunknown              /dev/tty        4    1-63 console\npty_slave            /dev/pts      136 0-1048575 pty:slave\npty_master           /dev/ptm      128 0-1048575 pty:master\n";
const RANDOM_POOL: &[u8] = b"256\n";

/// Failures exposed while resolving a synthetic process file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    NotFound,
    Access,
    ReadOnly,
    Invalid,
    ResourceLimit,
}

/// Process-filesystem namespace backed by typed domain snapshots.
pub struct Procfs {
    source: Arc<dyn Source>,
}

struct Identity([u8; 16]);

impl Identity {
    fn new(mut bytes: [u8; 16]) -> Self {
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Self(bytes)
    }

    fn into_bytes(self) -> Vec<u8> {
        let bytes = self.0;
        format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}\n",
            bytes[0],
            bytes[1],
            bytes[2],
            bytes[3],
            bytes[4],
            bytes[5],
            bytes[6],
            bytes[7],
            bytes[8],
            bytes[9],
            bytes[10],
            bytes[11],
            bytes[12],
            bytes[13],
            bytes[14],
            bytes[15],
        )
        .into_bytes()
    }
}

#[cfg(test)]
mod identity_test {
    use super::Identity;

    #[test]
    fn wire_contract() {
        let raw = [0xff; 16];
        let first = Identity::new(raw).into_bytes();
        let second = Identity::new(raw).into_bytes();

        assert_eq!(first, b"ffffffff-ffff-4fff-bfff-ffffffffffff\n");
        assert_eq!(second, first);
        assert_eq!(first.len(), 37);
    }
}

impl Procfs {
    #[must_use]
    pub fn new(source: Arc<dyn Source>) -> Self {
        Self { source }
    }

    pub fn read_link(&self, path: &[u8], current: u32) -> Result<Option<Vec<u8>>, Error> {
        self.read_link_for(path, current, current)
    }

    /// Rewrites a `/proc/thread-self/<leaf>` spelling onto the calling thread's
    /// own task directory, which the node parser already understands.
    fn thread_self(path: &[u8], current: u32, thread: u32) -> Option<Vec<u8>> {
        let rest = path
            .strip_prefix(b"/")
            .unwrap_or(path)
            .strip_prefix(b"proc/thread-self/")?;
        let mut rewritten = format!("/proc/{current}/task/{thread}/").into_bytes();
        rewritten.extend_from_slice(rest);
        Some(rewritten)
    }

    pub fn metadata_for(&self, path: &[u8], current: u32, thread: u32) -> Result<Option<OfdMetadata>, Error> {
        match Self::thread_self(path, current, thread) {
            Some(rewritten) => self.metadata(&rewritten, current),
            None => self.metadata(path, current),
        }
    }

    pub fn open_for(
        &self,
        path: &[u8],
        current: u32,
        thread: u32,
        intent: crate::OpenIntent,
    ) -> Result<Option<Arc<dyn hl_descriptor::OpenFileDescription>>, Error> {
        match Self::thread_self(path, current, thread) {
            Some(rewritten) => self.open(&rewritten, current, intent),
            None => self.open(path, current, intent),
        }
    }

    pub fn read_link_for(&self, path: &[u8], current: u32, thread: u32) -> Result<Option<Vec<u8>>, Error> {
        if let Some(rewritten) = Self::thread_self(path, current, thread) {
            return self.read_link_for(&rewritten, current, thread);
        }
        let path = path.strip_prefix(b"/").unwrap_or(path);
        if path == b"proc/self" {
            let identity = self.source.resolve_process(current)?;
            self.source.process(identity)?;
            return Ok(Some(current.to_string().into_bytes()));
        }
        if path == b"proc/thread-self" {
            let identity = self.source.resolve_process(current)?;
            self.source.resolve_thread(identity, Some(thread))?;
            return Ok(Some(format!("{current}/task/{thread}").into_bytes()));
        }
        if let Some((process, thread, model::Node::Root)) = model::Node::parse(path, current) {
            let identity = self.resolve_path_process(process, thread)?;
            return self.source.root(identity).map(Some);
        }
        if let Some((process, thread, model::Node::Cwd)) = model::Node::parse(path, current) {
            let identity = self.resolve_path_process(process, thread)?;
            return self.source.cwd(identity).map(Some);
        }
        if let Some((_, _, model::Node::MountsLink)) = model::Node::parse(path, current) {
            return Ok(Some(b"self/mounts".to_vec()));
        }
        if let Some((process, thread, model::Node::Exe)) = model::Node::parse(path, current) {
            let identity = self.resolve_path_process(process, thread)?;
            return self.source.executable(identity).map(Some);
        }
        if let Some((process, thread, model::Node::UtsNamespace)) = model::Node::parse(path, current) {
            let identity = self.resolve_path_process(process, thread)?;
            let namespace = self.source.uts(identity)?.namespace;
            return Ok(Some(format!("uts:[{namespace}]").into_bytes()));
        }
        if let Some((process, thread, model::Node::NetworkNamespace)) = model::Node::parse(path, current) {
            let identity = self.resolve_path_process(process, thread)?;
            let namespace = self.source.network(identity)?.generation;
            return Ok(Some(format!("net:[{namespace}]").into_bytes()));
        }
        if let Some((process, thread, node)) = model::Node::parse(path, current)
            && let Some((name, inode)) = node.static_namespace()
        {
            let identity = self.resolve_path_process(process, thread)?;
            self.source.process(identity)?;
            return Ok(Some(format!("{name}:[{inode}]").into_bytes()));
        }
        if let Some((process, thread, model::Node::MapFile(start, end))) = model::Node::parse(path, current) {
            let identity = self.resolve_path_process(process, thread)?;
            let target = self
                .source
                .address_space(identity)?
                .regions
                .into_iter()
                .find(|region| region.start == start && region.end == end && region.inode != 0)
                .and_then(|region| region.path)
                .ok_or(Error::NotFound)?;
            return Ok(Some(target));
        }
        let Some((process, thread, model::Node::FdLink(number))) = model::Node::parse(path, current) else {
            return Ok(None);
        };
        let identity = self.resolve_path_process(process, thread)?;
        let target = self
            .source
            .descriptor(identity, number)?
            .target
            .ok_or(Error::NotFound)?;
        Ok(Some(target))
    }

    fn validate_thread(
        &self,
        process: Option<model::ProcessIdentity>,
        thread: Option<u32>,
    ) -> Result<Option<model::ThreadIdentity>, Error> {
        match (process, thread) {
            (_, None) => Ok(None),
            (Some(process), Some(thread)) => self.source.resolve_thread(process, Some(thread)).map(Some),
            (None, Some(_)) => Err(Error::NotFound),
        }
    }

    fn membership(&self, process: u32, identity: model::ProcessIdentity) -> Result<Vec<u8>, Error> {
        // Pin the numeric projection to the resolved task generation before reading
        // the immutable cgroup snapshot. Unified membership bytes contain no task
        // generation, so the number is only an index after this liveness check.
        self.source.process(identity)?;
        self.source.cgroup()?.membership(process).ok_or(Error::NotFound)
    }

    fn namespace_metadata(
        &self,
        process: u32,
        node: model::Node,
        identity: model::ProcessIdentity,
    ) -> Result<OfdMetadata, Error> {
        let mut metadata = node.metadata(process);
        if node == model::Node::UtsNamespace {
            metadata.inode = self.source.uts(identity)?.namespace;
        } else if node == model::Node::NetworkNamespace {
            metadata.inode = self.source.network(identity)?.generation;
        } else if let Some((_, inode)) = node.static_namespace() {
            self.source.process(identity)?;
            metadata.inode = inode;
        } else {
            return Err(Error::NotFound);
        }
        Ok(metadata)
    }

    fn resolve_path_process(&self, process: u32, thread: Option<u32>) -> Result<model::ProcessIdentity, Error> {
        let identity = self.source.resolve_process(process)?;
        self.validate_thread(Some(identity), thread)?;
        Ok(identity)
    }

    /// Resolves one live UTS namespace magic link to its stable task-owned identity.
    pub fn uts_namespace(&self, path: &[u8], current: u32) -> Result<Option<u64>, Error> {
        let Some((process, thread, model::Node::UtsNamespace)) = model::Node::parse(path, current) else {
            return Ok(None);
        };
        let identity = self.resolve_path_process(process, thread)?;
        self.source.uts(identity).map(|view| Some(view.namespace))
    }

    pub fn namespace_inode(&self, path: &[u8], current: u32) -> Result<Option<u64>, Error> {
        let Some((_, _, node)) = model::Node::parse(path.strip_prefix(b"/").unwrap_or(path), current) else {
            return Ok(None);
        };
        let namespace = matches!(
            node,
            model::Node::UtsNamespace
                | model::Node::NetworkNamespace
                | model::Node::CgroupNamespace
                | model::Node::IpcNamespace
                | model::Node::MountNamespace
                | model::Node::PidNamespace
                | model::Node::TimeNamespace
                | model::Node::UserNamespace
        );
        if !namespace {
            return Ok(None);
        }
        self.metadata(path, current)
            .map(|metadata| metadata.map(|value| value.inode))
    }
}

#[cfg(test)]
mod test;
