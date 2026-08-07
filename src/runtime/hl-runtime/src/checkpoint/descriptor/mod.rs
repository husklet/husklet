use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use hl_checkpoint::{CheckpointImage, Section};
use hl_descriptor::{
    DESCRIPTOR_CHECKPOINT_VERSION, DescriptorCheckpointError, DescriptorEntryImage, DescriptorFlags,
    DescriptorGenerationImage, DescriptorObjectCheckpoint, DescriptorTable as HostDescriptorTable,
    DescriptorTableImage, ObjectKind, OpenDescriptionImage, StatusFlags,
};

use crate::{
    CheckpointParticipant, CheckpointRole, DescriptorImageSlot, PreparedExecParticipant, PreparedProcessImage,
};

mod directory;
mod file;

pub use directory::{DirectoryObjectCatalog, DirectoryObjectCheckpoint};
pub use file::{FileObjectCatalog, FileObjectCheckpoint};

const DEPENDENCIES: [CheckpointRole; 1] = [CheckpointRole::Task];
const OBJECT_KINDS: usize = 8;

/// Routes each broad OFD family to its owning durable object codec.
pub struct ObjectCatalog {
    codecs: [Option<Arc<dyn DescriptorObjectCheckpoint>>; OBJECT_KINDS],
}

impl ObjectCatalog {
    #[must_use]
    pub fn rejecting() -> Self {
        Self {
            codecs: std::array::from_fn(|_| None),
        }
    }

    #[must_use]
    pub fn bind(mut self, kind: ObjectKind, codec: Arc<dyn DescriptorObjectCheckpoint>) -> Self {
        self.codecs[Self::index(kind)] = Some(codec);
        self
    }

    const fn index(kind: ObjectKind) -> usize {
        match kind {
            ObjectKind::File => 0,
            ObjectKind::Directory => 1,
            ObjectKind::Socket => 2,
            ObjectKind::Pipe => 3,
            ObjectKind::Event => 4,
            ObjectKind::EventCounter => 5,
            ObjectKind::Poll => 6,
            ObjectKind::Other => 7,
        }
    }

    fn codec(&self, kind: ObjectKind) -> Result<&dyn DescriptorObjectCheckpoint, DescriptorCheckpointError> {
        self.codecs[Self::index(kind)]
            .as_deref()
            .ok_or(DescriptorCheckpointError::Object)
    }
}

impl DescriptorObjectCheckpoint for ObjectCatalog {
    fn snapshot_size_identity(
        &self,
        identity: hl_descriptor::DescriptionIdentity,
        object: &dyn hl_descriptor::OpenFileDescription,
    ) -> Result<usize, DescriptorCheckpointError> {
        self.codec(object.kind())?.snapshot_size_identity(identity, object)
    }

    fn snapshot_into_identity(
        &self,
        identity: hl_descriptor::DescriptionIdentity,
        object: &dyn hl_descriptor::OpenFileDescription,
        output: &mut [u8],
    ) -> Result<(), DescriptorCheckpointError> {
        self.codec(object.kind())?
            .snapshot_into_identity(identity, object, output)
    }

    fn snapshot_size(
        &self,
        identity: u64,
        object: &dyn hl_descriptor::OpenFileDescription,
    ) -> Result<usize, DescriptorCheckpointError> {
        self.codec(object.kind())?.snapshot_size(identity, object)
    }

    fn snapshot_into(
        &self,
        identity: u64,
        object: &dyn hl_descriptor::OpenFileDescription,
        output: &mut [u8],
    ) -> Result<(), DescriptorCheckpointError> {
        self.codec(object.kind())?.snapshot_into(identity, object, output)
    }

    fn rebind(
        &self,
        description: &OpenDescriptionImage,
    ) -> Result<Arc<dyn hl_descriptor::OpenFileDescription>, DescriptorCheckpointError> {
        self.codec(description.kind)?.rebind(description)
    }
}

pub struct Table {
    slot: Arc<DescriptorImageSlot>,
    staged: RwLock<Option<Arc<HostDescriptorTable>>>,
}

impl Table {
    #[must_use]
    pub fn new(table: Arc<HostDescriptorTable>) -> Self {
        Self {
            slot: Arc::new(DescriptorImageSlot::from_shared(table)),
            staged: RwLock::new(None),
        }
    }

    #[must_use]
    pub const fn from_slot(slot: Arc<DescriptorImageSlot>) -> Self {
        Self {
            slot,
            staged: RwLock::new(None),
        }
    }

    #[must_use]
    pub fn current(&self) -> Arc<HostDescriptorTable> {
        self.slot.current().1
    }

    pub(crate) fn staged(&self) -> Option<Arc<HostDescriptorTable>> {
        self.staged
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn stage(&self, table: Arc<HostDescriptorTable>) -> Result<(), ()> {
        let mut staged = self.staged.write().map_err(|_| ())?;
        if staged.is_some() {
            return Err(());
        }
        *staged = Some(table);
        Ok(())
    }

    fn clear_stage(&self, table: &Arc<HostDescriptorTable>) {
        let mut staged = self.staged.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if staged.as_ref().is_some_and(|value| Arc::ptr_eq(value, table)) {
            *staged = None;
        }
    }
}

struct RestoreState {
    previous: Arc<HostDescriptorTable>,
    replacement: Arc<HostDescriptorTable>,
    publication: PreparedProcessImage<Arc<HostDescriptorTable>>,
    committed: bool,
}

pub struct Participant {
    table: Arc<Table>,
    objects: Arc<dyn DescriptorObjectCheckpoint>,
    frozen: Mutex<Option<Arc<HostDescriptorTable>>>,
    validated: Mutex<Option<([u8; 32], DescriptorTableImage)>>,
    staged: Mutex<BTreeMap<u64, RestoreState>>,
    next: AtomicU64,
}

impl Participant {
    #[must_use]
    pub fn new(table: Arc<Table>, objects: Arc<dyn DescriptorObjectCheckpoint>) -> Self {
        Self {
            table,
            objects,
            frozen: Mutex::new(None),
            validated: Mutex::new(None),
            staged: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
        }
    }
}

impl CheckpointParticipant for Participant {
    fn role(&self) -> CheckpointRole {
        CheckpointRole::Descriptors
    }

    fn version(&self) -> u32 {
        DESCRIPTOR_CHECKPOINT_VERSION
    }

    fn dependencies(&self) -> &[CheckpointRole] {
        &DEPENDENCIES
    }

    fn freeze(&self) -> Result<(), ()> {
        if self.frozen.lock().map_err(|_| ())?.is_some() {
            return Err(());
        }
        let table = self.table.current();
        table.freeze_checkpoint();
        let mut frozen = self.frozen.lock().map_err(|_| ())?;
        if frozen.is_some() {
            table.thaw_checkpoint();
            return Err(());
        }
        *frozen = Some(table);
        Ok(())
    }

    fn snapshot(&self) -> Result<Vec<u8>, ()> {
        let frozen = self.frozen.lock().map_err(|_| ())?;
        let table = frozen.as_ref().ok_or(())?;
        let image = table.checkpoint_image(self.objects.as_ref()).map_err(|_| ())?;
        Codec::encode(&image).map_err(|_| ())
    }

    fn thaw(&self) -> Result<(), ()> {
        let table = self.frozen.lock().map_err(|_| ())?.take().ok_or(())?;
        table.thaw_checkpoint();
        Ok(())
    }

    fn validate(&self, image: &CheckpointImage, section: &Section) -> Result<(), ()> {
        let decoded = Codec::decode(section.bytes()).map_err(|_| ())?;
        *self.validated.lock().map_err(|_| ())? = Some((image.digest(), decoded));
        Ok(())
    }

    fn stage(&self, section: &Section) -> Result<u64, ()> {
        self.stage_image(Codec::decode(section.bytes()).map_err(|_| ())?)
    }

    fn stage_bound(&self, digest: [u8; 32], section: &Section) -> Result<u64, ()> {
        let image = self
            .validated
            .lock()
            .map_err(|_| ())?
            .take()
            .filter(|(validated, _)| *validated == digest)
            .map(|(_, image)| image)
            .map_or_else(|| Codec::decode(section.bytes()).map_err(|_| ()), Ok)?;
        self.stage_image(image)
    }

    fn commit(&self, reservation: u64) -> Result<(), ()> {
        let mut staged = self.staged.lock().map_err(|_| ())?;
        let state = staged.get_mut(&reservation).ok_or(())?;
        state.publication.publish().map_err(|_| ())?;
        state.committed = true;
        Ok(())
    }

    fn rollback(&self, reservation: u64) {
        let state = self
            .staged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&reservation);
        if let Some(mut state) = state {
            self.table.clear_stage(&state.replacement);
            if state.committed {
                state.publication.rollback();
            }
            state.previous.thaw_checkpoint();
            state.replacement.thaw_checkpoint();
        }
    }

    fn resume(&self, reservation: u64) -> Result<(), ()> {
        let mut state = self.staged.lock().map_err(|_| ())?.remove(&reservation).ok_or(())?;
        if !state.committed {
            return Err(());
        }
        self.table.clear_stage(&state.replacement);
        state.publication.finish();
        state.replacement.release_checkpoint_roots();
        state.replacement.thaw_checkpoint();
        state.previous.thaw_checkpoint();
        Ok(())
    }
}

impl Participant {
    fn stage_image(&self, image: DescriptorTableImage) -> Result<u64, ()> {
        let (generation, previous) = self.table.slot.current();
        previous.freeze_checkpoint();
        let result = (|| {
            let replacement =
                Arc::new(HostDescriptorTable::restore_checkpoint(&image, self.objects.as_ref()).map_err(|_| ())?);
            replacement.freeze_checkpoint();
            self.table.stage(replacement.clone())?;
            let publication = self.table.slot.prepare_checkpoint(generation, Arc::clone(&replacement));
            let reservation = self.next.fetch_add(1, Ordering::Relaxed);
            if reservation == 0 {
                self.table.clear_stage(&replacement);
                replacement.thaw_checkpoint();
                return Err(());
            }
            let Ok(mut staged) = self.staged.lock() else {
                self.table.clear_stage(&replacement);
                replacement.thaw_checkpoint();
                return Err(());
            };
            staged.insert(
                reservation,
                RestoreState {
                    previous: previous.clone(),
                    replacement,
                    publication,
                    committed: false,
                },
            );
            Ok(reservation)
        })();
        if result.is_err() {
            previous.thaw_checkpoint();
        }
        result
    }
}

struct Codec;

impl Codec {
    fn encode(image: &DescriptorTableImage) -> Result<Vec<u8>, DescriptorCheckpointError> {
        image.validate()?;
        let mut bytes = Vec::new();
        Self::u32(&mut bytes, image.version);
        Self::u32(&mut bytes, image.limit as u32);
        Self::u32(&mut bytes, image.generations.len() as u32);
        Self::u32(&mut bytes, image.descriptions.len() as u32);
        Self::u32(&mut bytes, image.entries.len() as u32);
        for value in &image.generations {
            Self::u32(&mut bytes, value.number as u32);
            Self::u32(&mut bytes, value.generation);
        }
        for value in &image.descriptions {
            Self::u64(&mut bytes, value.identity);
            Self::u32(&mut bytes, value.generation);
            Self::u64(&mut bytes, value.offset);
            Self::u32(&mut bytes, value.status.bits());
            bytes.push(Self::kind(value.kind));
            bytes.extend_from_slice(&[0; 3]);
            Self::u32(&mut bytes, value.object.len() as u32);
            bytes.extend_from_slice(&value.object);
        }
        for value in &image.entries {
            Self::u32(&mut bytes, value.number as u32);
            Self::u32(&mut bytes, value.generation);
            Self::u32(&mut bytes, value.flags.bits());
            Self::u64(&mut bytes, value.description_identity);
        }
        Ok(bytes)
    }

    fn decode(bytes: &[u8]) -> Result<DescriptorTableImage, DescriptorCheckpointError> {
        let mut input = Input { bytes, offset: 0 };
        let version = input.u32()?;
        let limit = i32::try_from(input.u32()?).map_err(|_| DescriptorCheckpointError::Limit)?;
        let generations = input.count()?;
        let descriptions = input.count()?;
        let entries = input.count()?;
        let mut generation_values = Vec::with_capacity(generations);
        for _ in 0..generations {
            generation_values.push(DescriptorGenerationImage {
                number: i32::try_from(input.u32()?).map_err(|_| DescriptorCheckpointError::Limit)?,
                generation: input.u32()?,
            });
        }
        let mut description_values = Vec::with_capacity(descriptions);
        for _ in 0..descriptions {
            let identity = input.u64()?;
            let generation = input.u32()?;
            let offset = input.u64()?;
            let status = StatusFlags::from_bits(input.u32()?);
            let kind = input.kind()?;
            input.reserved()?;
            let object = input.vector()?;
            description_values.push(OpenDescriptionImage {
                identity,
                generation,
                offset,
                status,
                kind,
                object,
            });
        }
        let mut entry_values = Vec::with_capacity(entries);
        for _ in 0..entries {
            entry_values.push(DescriptorEntryImage {
                number: i32::try_from(input.u32()?).map_err(|_| DescriptorCheckpointError::Limit)?,
                generation: input.u32()?,
                flags: DescriptorFlags::from_bits(input.u32()?),
                description_identity: input.u64()?,
            });
        }
        if input.offset != bytes.len() {
            return Err(DescriptorCheckpointError::Limit);
        }
        let image = DescriptorTableImage {
            version,
            limit,
            generations: generation_values,
            descriptions: description_values,
            entries: entry_values,
        };
        image.validate()?;
        Ok(image)
    }

    fn kind(kind: ObjectKind) -> u8 {
        match kind {
            ObjectKind::File => 1,
            ObjectKind::Directory => 2,
            ObjectKind::Socket => 3,
            ObjectKind::Pipe => 4,
            ObjectKind::Event => 5,
            ObjectKind::EventCounter => 8,
            ObjectKind::Poll => 6,
            ObjectKind::Other => 7,
        }
    }

    fn u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    fn u64(bytes: &mut Vec<u8>, value: u64) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}

struct Input<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Input<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], DescriptorCheckpointError> {
        let end = self.offset.checked_add(count).ok_or(DescriptorCheckpointError::Limit)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(DescriptorCheckpointError::Limit)?;
        self.offset = end;
        Ok(value)
    }

    fn u32(&mut self) -> Result<u32, DescriptorCheckpointError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, DescriptorCheckpointError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn count(&mut self) -> Result<usize, DescriptorCheckpointError> {
        let count = usize::try_from(self.u32()?).map_err(|_| DescriptorCheckpointError::Limit)?;
        if count > hl_descriptor::DESCRIPTOR_CHECKPOINT_MAXIMUM {
            return Err(DescriptorCheckpointError::Limit);
        }
        Ok(count)
    }

    fn kind(&mut self) -> Result<ObjectKind, DescriptorCheckpointError> {
        Ok(match self.take(1)?[0] {
            1 => ObjectKind::File,
            2 => ObjectKind::Directory,
            3 => ObjectKind::Socket,
            4 => ObjectKind::Pipe,
            5 => ObjectKind::Event,
            8 => ObjectKind::EventCounter,
            6 => ObjectKind::Poll,
            7 => ObjectKind::Other,
            _ => return Err(DescriptorCheckpointError::Object),
        })
    }

    fn reserved(&mut self) -> Result<(), DescriptorCheckpointError> {
        if self.take(3)? != [0; 3] {
            return Err(DescriptorCheckpointError::Object);
        }
        Ok(())
    }

    fn vector(&mut self) -> Result<Vec<u8>, DescriptorCheckpointError> {
        let count = usize::try_from(self.u32()?).map_err(|_| DescriptorCheckpointError::Limit)?;
        if count > hl_descriptor::DESCRIPTION_CHECKPOINT_BYTES_MAXIMUM {
            return Err(DescriptorCheckpointError::Limit);
        }
        Ok(self.take(count)?.to_vec())
    }
}
