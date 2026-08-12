use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_descriptor::{
    DescriptionIdentity, DescriptorError, DescriptorFlags, DescriptorSnapshot, DescriptorTable, ExactDuplicate,
    ObjectKind, OpenFileDescription, OperationCancellation, StatusFlags,
};
use hl_event::{
    Epoll, EpollBatch, EpollError, EpollEvent, EpollInterest, EpollWatchKey, Inotify, InotifyError, InotifyLimits,
    SignalFd, SignalFdError, SignalFdFlags, SignalMask, SignalQueue, WatchSource,
};

pub struct RuntimeEpollBatch {
    epoll: Arc<Epoll>,
    batch: EpollBatch,
}

impl RuntimeEpollBatch {
    #[must_use]
    pub fn events(&self) -> &[EpollEvent] {
        self.batch.events()
    }
}

use crate::{GraphError, OwnershipGraph};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DescriptorTableId(u64);

pub struct RuntimeDescriptorTable {
    id: DescriptorTableId,
    slot: Arc<crate::DescriptorImageSlot>,
    limit: i32,
}

impl RuntimeDescriptorTable {
    #[must_use]
    pub const fn id(&self) -> DescriptorTableId {
        self.id
    }

    #[must_use]
    pub fn descriptor_table(&self) -> Arc<DescriptorTable> {
        self.slot.current().1
    }

    #[must_use]
    pub fn image_slot(&self) -> Arc<crate::DescriptorImageSlot> {
        Arc::clone(&self.slot)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlError {
    Capacity,
    Descriptor(DescriptorError),
    Epoll(EpollError),
    Graph(GraphError),
    SignalFd(SignalFdError),
    Inotify(InotifyError),
    NotEpoll,
}

impl From<DescriptorError> for ControlError {
    fn from(error: DescriptorError) -> Self {
        Self::Descriptor(error)
    }
}

impl From<EpollError> for ControlError {
    fn from(error: EpollError) -> Self {
        Self::Epoll(error)
    }
}

impl From<GraphError> for ControlError {
    fn from(error: GraphError) -> Self {
        Self::Graph(error)
    }
}

pub(crate) struct ControlState {
    next_table: u64,
    pub(crate) epolls: BTreeMap<DescriptionIdentity, Arc<Epoll>>,
}

/// Narrow coordinator for descriptor operations that affect event ownership.
pub struct Control {
    pub(crate) mutation: Mutex<()>,
    pub(crate) state: Mutex<ControlState>,
    pub(crate) graph: OwnershipGraph,
}

impl Control {
    /// Performs the non-mutating descriptor admission for `epoll_ctl`.
    pub fn admit(
        &self,
        table: &RuntimeDescriptorTable,
        epoll_number: i32,
        target_number: i32,
    ) -> Result<(), ControlError> {
        let descriptors = table.descriptor_table();
        let _source = descriptors.pin(epoll_number)?;
        if epoll_number == target_number {
            return Err(ControlError::Epoll(EpollError::InvalidArgument));
        }
        let target = descriptors.pin(target_number)?;
        if matches!(target.object().kind(), ObjectKind::File | ObjectKind::Directory) {
            return Err(ControlError::Epoll(EpollError::TargetUnavailable));
        }
        Ok(())
    }

    pub fn new(descriptor_limit: i32, graph_limit: usize) -> Result<(Self, RuntimeDescriptorTable), ControlError> {
        let table = Arc::new(DescriptorTable::new(descriptor_limit)?);
        let slot = Arc::new(crate::DescriptorImageSlot::from_shared(table));
        Ok((
            Self {
                mutation: Mutex::new(()),
                state: Mutex::new(ControlState {
                    next_table: 2,
                    epolls: BTreeMap::new(),
                }),
                graph: OwnershipGraph::new(graph_limit)?,
            },
            RuntimeDescriptorTable {
                id: DescriptorTableId(1),
                slot,
                limit: descriptor_limit,
            },
        ))
    }

    pub fn attach(
        table: Arc<DescriptorTable>,
        descriptor_limit: i32,
        graph_limit: usize,
    ) -> Result<(Self, RuntimeDescriptorTable), ControlError> {
        Ok((
            Self {
                mutation: Mutex::new(()),
                state: Mutex::new(ControlState {
                    next_table: 2,
                    epolls: BTreeMap::new(),
                }),
                graph: OwnershipGraph::new(graph_limit)?,
            },
            RuntimeDescriptorTable {
                id: DescriptorTableId(1),
                slot: Arc::new(crate::DescriptorImageSlot::from_shared(table)),
                limit: descriptor_limit,
            },
        ))
    }

    pub fn register_epoll(&self, identity: DescriptionIdentity, epoll: Arc<Epoll>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .epolls
            .insert(identity, epoll);
    }

    pub fn retire_identity(&self, identity: DescriptionIdentity) {
        // Catalog-bound descriptions invoke this from their final close while
        // the descriptor mutation transaction is already held. Re-locking the
        // non-reentrant mutex would deadlock close(2).
        self.retire(identity);
    }

    pub fn wait(
        &self,
        table: &RuntimeDescriptorTable,
        epoll_number: i32,
        maximum: usize,
        timeout: Option<std::time::Duration>,
    ) -> Result<Vec<EpollEvent>, ControlError> {
        let source = table.descriptor_table().pin(epoll_number)?;
        let epoll = self.epoll(source.description_identity())?;
        Ok(epoll.wait(maximum, timeout)?)
    }

    pub fn peek_wait(
        &self,
        table: &RuntimeDescriptorTable,
        epoll_number: i32,
        maximum: usize,
        timeout: Option<std::time::Duration>,
    ) -> Result<RuntimeEpollBatch, ControlError> {
        let source = table.descriptor_table().pin(epoll_number)?;
        let epoll = self.epoll(source.description_identity())?;
        let batch = epoll.peek_wait(maximum, timeout)?;
        Ok(RuntimeEpollBatch { epoll, batch })
    }

    pub fn peek_wait_interruptible(
        &self,
        table: &RuntimeDescriptorTable,
        epoll_number: i32,
        maximum: usize,
        timeout: Option<std::time::Duration>,
        cancellation: &dyn OperationCancellation,
    ) -> Result<RuntimeEpollBatch, ControlError> {
        let source = table.descriptor_table().pin(epoll_number)?;
        let epoll = self.epoll(source.description_identity())?;
        let batch = epoll.peek_wait_interruptible(maximum, timeout, cancellation)?;
        Ok(RuntimeEpollBatch { epoll, batch })
    }

    pub fn commit_wait(&self, batch: RuntimeEpollBatch) -> Result<bool, ControlError> {
        Ok(batch.epoll.commit(batch.batch)?)
    }

    pub fn create_epoll(&self, table: &RuntimeDescriptorTable, flags: DescriptorFlags) -> Result<i32, ControlError> {
        self.create_with_limit(table, flags, 4_096)
    }

    pub fn create_with_limit(
        &self,
        table: &RuntimeDescriptorTable,
        flags: DescriptorFlags,
        watch_limit: usize,
    ) -> Result<i32, ControlError> {
        let _mutation = self.mutation.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let epoll = Arc::new(Epoll::with_watch_limit(watch_limit)?);
        let number = Self::install_object(table, epoll.clone(), flags)?;
        let identity = table.descriptor_table().pin(number)?.description_identity();
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .epolls
            .insert(identity, epoll);
        Ok(number)
    }

    pub fn create_signalfd(
        &self,
        table: &RuntimeDescriptorTable,
        mask: SignalMask,
        flags: SignalFdFlags,
        queue: Arc<dyn SignalQueue>,
    ) -> Result<i32, ControlError> {
        let _mutation = self.mutation.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let closes = if flags.closes_on_exec() {
            DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC)
        } else {
            DescriptorFlags::default()
        };
        let object = Arc::new(SignalFd::new(mask, flags, queue).map_err(ControlError::SignalFd)?);
        Self::install_object(table, object, closes)
    }

    pub fn create_inotify(
        &self,
        table: &RuntimeDescriptorTable,
        nonblocking: bool,
        close_on_exec: bool,
        limits: InotifyLimits,
        source: Arc<dyn WatchSource>,
    ) -> Result<i32, ControlError> {
        let _mutation = self.mutation.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let flags = if close_on_exec {
            DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC)
        } else {
            DescriptorFlags::default()
        };
        let object = Arc::new(Inotify::new(nonblocking, limits, source).map_err(ControlError::Inotify)?);
        Self::install_object(table, object, flags)
    }

    pub fn add(
        &self,
        table: &RuntimeDescriptorTable,
        epoll_number: i32,
        target_number: i32,
        interests: EpollInterest,
        data: u64,
    ) -> Result<EpollWatchKey, ControlError> {
        let _mutation = self.mutation.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let source = table.descriptor_table().pin(epoll_number)?;
        let target = table.descriptor_table().pin(target_number)?;
        let epoll = self.epoll(source.description_identity())?;
        self.graph
            .add(&source, &epoll, target, interests, data)
            .map_err(ControlError::Graph)
    }

    pub fn modify(
        &self,
        table: &RuntimeDescriptorTable,
        epoll_number: i32,
        target_number: i32,
        interests: EpollInterest,
        data: u64,
    ) -> Result<(), ControlError> {
        let _mutation = self.mutation.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let source = table.descriptor_table().pin(epoll_number)?;
        let target = table.descriptor_table().pin(target_number)?;
        self.epoll(source.description_identity())?
            .modify(&target, interests, data)?;
        Ok(())
    }

    pub fn delete(
        &self,
        table: &RuntimeDescriptorTable,
        epoll_number: i32,
        target_number: i32,
    ) -> Result<(), ControlError> {
        let _mutation = self.mutation.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let source = table.descriptor_table().pin(epoll_number)?;
        let target = table.descriptor_table().pin(target_number)?;
        let epoll = self.epoll(source.description_identity())?;
        self.graph.delete(&source, &epoll, &target)?;
        Ok(())
    }

    pub fn close(&self, table: &RuntimeDescriptorTable, number: i32) -> Result<(), ControlError> {
        let _mutation = self.mutation.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let descriptors = table.descriptor_table();
        let snapshot = descriptors.snapshot(number)?;
        descriptors.close(number)?;
        if snapshot.descriptor_references == 1 {
            self.retire(Self::identity(snapshot));
        }
        Ok(())
    }

    pub fn duplicate(
        &self,
        table: &RuntimeDescriptorTable,
        source: i32,
        minimum: i32,
        flags: DescriptorFlags,
    ) -> Result<i32, ControlError> {
        let _mutation = self.mutation.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(table.descriptor_table().duplicate(source, minimum, flags)?)
    }

    pub fn duplicate_exact(
        &self,
        table: &RuntimeDescriptorTable,
        source: i32,
        destination: i32,
        operation: ExactDuplicate,
    ) -> Result<i32, ControlError> {
        let _mutation = self.mutation.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let descriptors = table.descriptor_table();
        let replaced = descriptors.snapshot(destination).ok();
        let result = descriptors.duplicate_exact(source, destination, operation)?;
        if let Some(snapshot) = replaced {
            let source_identity = Self::identity(descriptors.snapshot(source)?);
            if Self::identity(snapshot) != source_identity && snapshot.descriptor_references == 1 {
                self.retire(Self::identity(snapshot));
            }
        }
        Ok(result)
    }

    pub fn fork(&self, table: &RuntimeDescriptorTable) -> RuntimeDescriptorTable {
        let _mutation = self.mutation.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let id = DescriptorTableId(state.next_table);
            state.next_table = state.next_table.wrapping_add(1).max(2);
            id
        };
        RuntimeDescriptorTable {
            id,
            slot: Arc::new(crate::DescriptorImageSlot::from_shared(Arc::new(
                table.descriptor_table().fork(),
            ))),
            limit: table.limit,
        }
    }

    pub fn share(&self, table: &RuntimeDescriptorTable) -> RuntimeDescriptorTable {
        let _mutation = self.mutation.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let id = DescriptorTableId(state.next_table);
            state.next_table = state.next_table.wrapping_add(1).max(2);
            id
        };
        RuntimeDescriptorTable {
            id,
            slot: table.image_slot(),
            limit: table.limit,
        }
    }

    /// Builds the unpublished descriptor image used by a replacement process
    /// context while preserving the logical table identity and limits.
    #[must_use]
    pub fn exec_image(
        &self,
        source: &RuntimeDescriptorTable,
        candidate: Arc<DescriptorTable>,
    ) -> RuntimeDescriptorTable {
        RuntimeDescriptorTable {
            id: source.id,
            limit: source.limit,
            slot: Arc::new(crate::DescriptorImageSlot::from_shared(candidate)),
        }
    }

    pub fn exec_sweep(&self, table: &RuntimeDescriptorTable) -> Vec<i32> {
        let _mutation = self.mutation.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let snapshots = Self::snapshots(table);
        let closed = table.descriptor_table().close_on_exec();
        let mut closed_counts = BTreeMap::<DescriptionIdentity, u32>::new();
        for snapshot in snapshots.iter().filter(|snapshot| closed.contains(&snapshot.number)) {
            *closed_counts.entry(Self::identity(*snapshot)).or_default() += 1;
        }
        for snapshot in snapshots {
            let identity = Self::identity(snapshot);
            if closed_counts.get(&identity) == Some(&snapshot.descriptor_references) {
                self.retire(identity);
            }
        }
        closed
    }

    #[must_use]
    pub fn graph_snapshot(&self) -> crate::GraphSnapshot {
        self.graph.snapshot()
    }

    pub fn snapshot(&self, table: &RuntimeDescriptorTable, number: i32) -> Result<DescriptorSnapshot, ControlError> {
        Ok(table.descriptor_table().snapshot(number)?)
    }

    fn install_object<T: OpenFileDescription>(
        table: &RuntimeDescriptorTable,
        object: Arc<T>,
        flags: DescriptorFlags,
    ) -> Result<i32, ControlError> {
        let descriptors = table.descriptor_table();
        let reservation = descriptors.reserve(0)?;
        Ok(descriptors.commit(reservation, object, StatusFlags::default(), flags)?)
    }

    fn epoll(&self, identity: DescriptionIdentity) -> Result<Arc<Epoll>, ControlError> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .epolls
            .get(&identity)
            .cloned()
            .ok_or(ControlError::NotEpoll)
    }

    fn retire(&self, identity: DescriptionIdentity) {
        self.graph.close(identity);
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .epolls
            .remove(&identity);
    }

    fn snapshots(table: &RuntimeDescriptorTable) -> Vec<DescriptorSnapshot> {
        (0..table.limit)
            .filter_map(|number| table.descriptor_table().snapshot(number).ok())
            .collect()
    }

    const fn identity(snapshot: DescriptorSnapshot) -> DescriptionIdentity {
        DescriptionIdentity {
            identity: snapshot.description_identity,
            generation: snapshot.description_generation,
        }
    }
}
