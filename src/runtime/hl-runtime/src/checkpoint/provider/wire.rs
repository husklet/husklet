use hl_descriptor::{Readiness, StatusFlags};
use hl_provider::{
    FileAccess, FileSnapshot, HandleKind, NamespaceSnapshot, ProviderCheckpointImage, ProviderClientCheckpoint,
    ProviderFileCheckpoint, ProviderResourceKey, ProviderResourceReference, ProviderSubscriptionCheckpoint, RemoteId,
    SnapshotEntry,
};

use super::ProviderCheckpointCodec;

const WIRE_VERSION: u32 = 1;
pub const PROVIDER_CHECKPOINT_BYTES_MAXIMUM: usize = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default)]
pub struct PortableProviderCodec;

impl ProviderCheckpointCodec for PortableProviderCodec {
    fn encode(&self, image: &ProviderCheckpointImage) -> Result<Vec<u8>, ()> {
        image.validate().map_err(|_| ())?;
        let mut output = Output::default();
        output.u32(WIRE_VERSION)?;
        output.u32(image.version)?;
        output.namespace(&image.namespace)?;
        output.count(image.resources.len())?;
        for resource in &image.resources {
            output.count(resource.slot)?;
            output.key(resource.key)?;
        }
        output.count(image.files.len())?;
        for file in &image.files {
            output.file(file)?;
        }
        output.client(&image.client)?;
        Ok(output.bytes)
    }

    fn decode(&self, bytes: &[u8]) -> Result<ProviderCheckpointImage, ()> {
        if bytes.len() > PROVIDER_CHECKPOINT_BYTES_MAXIMUM {
            return Err(());
        }
        let mut input = Input { bytes, offset: 0 };
        if input.u32()? != WIRE_VERSION {
            return Err(());
        }
        let version = input.u32()?;
        let namespace = input.namespace()?;
        let resource_count = input.count()?;
        let mut resources = Vec::with_capacity(resource_count);
        for _ in 0..resource_count {
            resources.push(ProviderResourceReference {
                slot: input.count()?,
                key: input.key()?,
            });
        }
        let file_count = input.count()?;
        let mut files = Vec::with_capacity(file_count);
        for _ in 0..file_count {
            files.push(input.file()?);
        }
        let client = input.client()?;
        if input.offset != bytes.len() {
            return Err(());
        }
        let image = ProviderCheckpointImage {
            version,
            namespace,
            resources,
            files,
            client,
        };
        image.validate().map_err(|_| ())?;
        Ok(image)
    }
}

#[derive(Default)]
struct Output {
    bytes: Vec<u8>,
}

impl Output {
    fn extend(&mut self, bytes: &[u8]) -> Result<(), ()> {
        let length = self.bytes.len().checked_add(bytes.len()).ok_or(())?;
        if length > PROVIDER_CHECKPOINT_BYTES_MAXIMUM {
            return Err(());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn u8(&mut self, value: u8) -> Result<(), ()> {
        self.extend(&[value])
    }
    fn u16(&mut self, value: u16) -> Result<(), ()> {
        self.extend(&value.to_le_bytes())
    }
    fn u32(&mut self, value: u32) -> Result<(), ()> {
        self.extend(&value.to_le_bytes())
    }
    fn u64(&mut self, value: u64) -> Result<(), ()> {
        self.extend(&value.to_le_bytes())
    }

    fn count(&mut self, value: usize) -> Result<(), ()> {
        self.u32(u32::try_from(value).map_err(|_| ())?)
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), ()> {
        self.count(value.len())?;
        self.extend(value)
    }

    fn key(&mut self, key: ProviderResourceKey) -> Result<(), ()> {
        self.u64(key.get())
    }

    fn kind(&mut self, kind: HandleKind) -> Result<(), ()> {
        self.u8(match kind {
            HandleKind::File => 1,
            HandleKind::Directory => 2,
            HandleKind::Mapping => 3,
            HandleKind::Process => 4,
            HandleKind::Event => 5,
            HandleKind::Counter => 6,
            HandleKind::Subscription => 7,
            HandleKind::Transfer => 8,
        })
    }

    fn namespace(&mut self, value: &NamespaceSnapshot) -> Result<(), ()> {
        self.count(value.capacity)?;
        self.count(value.live)?;
        self.u64(value.references)?;
        self.count(value.generations.len())?;
        for generation in &value.generations {
            self.u16(*generation)?;
        }
        self.count(value.entries.len())?;
        for entry in &value.entries {
            self.count(entry.slot)?;
            self.u16(entry.generation)?;
            self.u64(entry.remote.get())?;
            self.kind(entry.kind)?;
            self.u32(entry.references)?;
        }
        Ok(())
    }

    fn access(&mut self, access: FileAccess) -> Result<(), ()> {
        self.u8(match access {
            FileAccess::Read => 1,
            FileAccess::Write => 2,
            FileAccess::ReadWrite => 3,
        })
    }

    fn snapshot(&mut self, value: &FileSnapshot) -> Result<(), ()> {
        self.u64(value.remote.get())?;
        self.u64(value.service)?;
        self.access(value.access)?;
        self.u32(value.status.bits())?;
        self.u64(value.offset)?;
        self.u32(value.readiness.bits())?;
        self.u64(value.identity_namespace)?;
        self.bytes(&value.path)
    }

    fn file(&mut self, value: &ProviderFileCheckpoint) -> Result<(), ()> {
        self.key(value.descriptor)?;
        self.key(value.resource)?;
        self.snapshot(&value.snapshot)
    }

    fn client(&mut self, value: &ProviderClientCheckpoint) -> Result<(), ()> {
        self.count(value.request_generations.len())?;
        for generation in &value.request_generations {
            self.u32(*generation)?;
        }
        self.count(value.subscription_generations.len())?;
        for generation in &value.subscription_generations {
            self.u32(*generation)?;
        }
        self.u64(value.next_request)?;
        self.u64(value.next_subscription)?;
        self.u64(value.late_replies)?;
        self.u64(value.stale_events)?;
        self.count(value.subscriptions.len())?;
        for subscription in &value.subscriptions {
            self.count(subscription.slot)?;
            self.u64(subscription.identity_owner)?;
            self.u32(subscription.identity_generation)?;
            self.u64(subscription.key_id)?;
            self.u32(subscription.key_generation)?;
            self.count(subscription.queued.len())?;
            for event in &subscription.queued {
                self.bytes(event)?;
            }
            self.u64(subscription.lost)?;
        }
        Ok(())
    }
}

struct Input<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Input<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], ()> {
        let end = self.offset.checked_add(count).ok_or(())?;
        let value = self.bytes.get(self.offset..end).ok_or(())?;
        self.offset = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, ()> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, ()> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().map_err(|_| ())?))
    }
    fn u32(&mut self) -> Result<u32, ()> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| ())?))
    }
    fn u64(&mut self) -> Result<u64, ()> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().map_err(|_| ())?))
    }

    fn count(&mut self) -> Result<usize, ()> {
        let count = usize::try_from(self.u32()?).map_err(|_| ())?;
        if count > self.bytes.len().saturating_sub(self.offset) {
            return Err(());
        }
        Ok(count)
    }

    fn owned(&mut self) -> Result<Vec<u8>, ()> {
        let count = self.count()?;
        Ok(self.take(count)?.to_vec())
    }

    fn key(&mut self) -> Result<ProviderResourceKey, ()> {
        ProviderResourceKey::new(self.u64()?).ok_or(())
    }

    fn kind(&mut self) -> Result<HandleKind, ()> {
        match self.u8()? {
            1 => Ok(HandleKind::File),
            2 => Ok(HandleKind::Directory),
            3 => Ok(HandleKind::Mapping),
            4 => Ok(HandleKind::Process),
            5 => Ok(HandleKind::Event),
            6 => Ok(HandleKind::Counter),
            7 => Ok(HandleKind::Subscription),
            8 => Ok(HandleKind::Transfer),
            _ => Err(()),
        }
    }

    fn namespace(&mut self) -> Result<NamespaceSnapshot, ()> {
        let capacity = self.count()?;
        let live = self.count()?;
        let references = self.u64()?;
        let generation_count = self.count()?;
        let mut generations = Vec::with_capacity(generation_count);
        for _ in 0..generation_count {
            generations.push(self.u16()?);
        }
        let entry_count = self.count()?;
        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            entries.push(SnapshotEntry {
                slot: self.count()?,
                generation: self.u16()?,
                remote: RemoteId::new(self.u64()?).ok_or(())?,
                kind: self.kind()?,
                references: self.u32()?,
            });
        }
        Ok(NamespaceSnapshot {
            capacity,
            live,
            references,
            generations,
            entries,
        })
    }

    fn access(&mut self) -> Result<FileAccess, ()> {
        match self.u8()? {
            1 => Ok(FileAccess::Read),
            2 => Ok(FileAccess::Write),
            3 => Ok(FileAccess::ReadWrite),
            _ => Err(()),
        }
    }

    fn snapshot(&mut self) -> Result<FileSnapshot, ()> {
        Ok(FileSnapshot {
            remote: RemoteId::new(self.u64()?).ok_or(())?,
            service: self.u64()?,
            access: self.access()?,
            status: StatusFlags::from_bits(self.u32()?),
            offset: self.u64()?,
            readiness: Readiness::from_bits(self.u32()?),
            identity_namespace: self.u64()?,
            path: self.owned()?,
        })
    }

    fn file(&mut self) -> Result<ProviderFileCheckpoint, ()> {
        Ok(ProviderFileCheckpoint {
            descriptor: self.key()?,
            resource: self.key()?,
            snapshot: self.snapshot()?,
        })
    }

    fn client(&mut self) -> Result<ProviderClientCheckpoint, ()> {
        let request_count = self.count()?;
        let mut request_generations = Vec::with_capacity(request_count);
        for _ in 0..request_count {
            request_generations.push(self.u32()?);
        }
        let subscription_count = self.count()?;
        let mut subscription_generations = Vec::with_capacity(subscription_count);
        for _ in 0..subscription_count {
            subscription_generations.push(self.u32()?);
        }
        let next_request = self.u64()?;
        let next_subscription = self.u64()?;
        let late_replies = self.u64()?;
        let stale_events = self.u64()?;
        let live_count = self.count()?;
        let mut subscriptions = Vec::with_capacity(live_count);
        for _ in 0..live_count {
            subscriptions.push(self.subscription()?);
        }
        Ok(ProviderClientCheckpoint {
            request_generations,
            subscription_generations,
            next_request,
            next_subscription,
            late_replies,
            stale_events,
            subscriptions,
        })
    }

    fn subscription(&mut self) -> Result<ProviderSubscriptionCheckpoint, ()> {
        let slot = self.count()?;
        let identity_owner = self.u64()?;
        let identity_generation = self.u32()?;
        let key_id = self.u64()?;
        let key_generation = self.u32()?;
        let event_count = self.count()?;
        let mut queued = Vec::with_capacity(event_count);
        for _ in 0..event_count {
            queued.push(self.owned()?);
        }
        Ok(ProviderSubscriptionCheckpoint {
            slot,
            identity_owner,
            identity_generation,
            key_id,
            key_generation,
            queued,
            lost: self.u64()?,
        })
    }
}
