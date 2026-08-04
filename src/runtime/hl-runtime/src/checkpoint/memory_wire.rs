use std::io::Write;

use hl_isa::{AddressRange, GuestAddress};
use hl_memory::{
    Backing, FileIdentity, MEMORY_CHECKPOINT_BYTES_MAXIMUM, MemoryCheckpointImage, MemoryLedgerSnapshot,
    MemoryMappingSnapshot, Protection, Region, SharedBackingRef, SharedLimits, SharedObjectId, SharedObjectSnapshot,
    SharedSeal, SharedStoreSnapshot,
};
use serde::{Deserialize, Serialize};

const MAGIC: u32 = 0x4d45_4d48;
const WIRE_VERSION: u32 = 1;
const HEADER_LENGTH: usize = 24;

struct BoundedBytes(Vec<u8>);

impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let length = self
            .0
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("memory checkpoint overflow"))?;
        if length > MEMORY_CHECKPOINT_BYTES_MAXIMUM {
            return Err(std::io::Error::other("memory checkpoint limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryWire {
    memory: u32,
    address_limit: u64,
    limits: [u64; 3],
    generations: Vec<u32>,
    objects: Vec<ObjectWire>,
    ledger_generation: u64,
    regions: Vec<RegionWire>,
    mappings: Vec<MappingWire>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ObjectWire {
    slot: u32,
    generation: u32,
    owner: u64,
    seals: u8,
    bytes: Vec<u8>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RegionWire {
    start: u64,
    length: u64,
    protection: u8,
    backing: BackingWire,
    offset: u64,
}

#[derive(Deserialize, Serialize)]
enum BackingWire {
    Shared {
        slot: u32,
        generation: u32,
        offset: u64,
        length: u64,
        #[serde(default)]
        write_shared: bool,
    },
    Anonymous {
        identity: u64,
        shared: bool,
    },
    File {
        device: u64,
        object: u64,
        shared: bool,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MappingWire {
    region: usize,
    bytes: Vec<u8>,
}

impl MemoryWire {
    pub(super) fn encode(image: &MemoryCheckpointImage) -> Result<Vec<u8>, ()> {
        image.validate().map_err(|_| ())?;
        let wire = Self::from_image(image)?;
        let mut payload = BoundedBytes(Vec::new());
        serde_json::to_writer(&mut payload, &wire).map_err(|_| ())?;
        let payload = payload.0;
        let length = u64::try_from(payload.len()).map_err(|_| ())?;
        let mut bytes = Vec::with_capacity(HEADER_LENGTH.checked_add(payload.len()).ok_or(())?);
        bytes.extend_from_slice(&MAGIC.to_le_bytes());
        bytes.extend_from_slice(&WIRE_VERSION.to_le_bytes());
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend_from_slice(&Self::checksum(&payload).to_le_bytes());
        bytes.extend_from_slice(&payload);
        Ok(bytes)
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<MemoryCheckpointImage, ()> {
        if bytes.len() < HEADER_LENGTH || bytes.len() > MEMORY_CHECKPOINT_BYTES_MAXIMUM {
            return Err(());
        }
        let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        if word(0) != MAGIC || word(4) != WIRE_VERSION {
            return Err(());
        }
        let length = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let length = usize::try_from(length).map_err(|_| ())?;
        if HEADER_LENGTH.checked_add(length) != Some(bytes.len()) {
            return Err(());
        }
        let expected = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let payload = &bytes[HEADER_LENGTH..];
        if Self::checksum(payload) != expected {
            return Err(());
        }
        let wire: Self = serde_json::from_slice(payload).map_err(|_| ())?;
        let image = wire.into_image()?;
        image.validate().map_err(|_| ())?;
        Ok(image)
    }

    fn from_image(image: &MemoryCheckpointImage) -> Result<Self, ()> {
        Ok(Self {
            memory: image.version,
            address_limit: image.address_limit,
            limits: [
                image.shared_limits.objects.try_into().map_err(|_| ())?,
                image.shared_limits.object_bytes.try_into().map_err(|_| ())?,
                image.shared_limits.total_bytes.try_into().map_err(|_| ())?,
            ],
            generations: image.shared.generations.clone(),
            objects: image.shared.objects.iter().map(ObjectWire::from_value).collect(),
            ledger_generation: image.ledger.generation,
            regions: image
                .ledger
                .regions
                .iter()
                .copied()
                .map(RegionWire::from_value)
                .collect(),
            mappings: image
                .mappings
                .iter()
                .map(|mapping| MappingWire {
                    region: mapping.region,
                    bytes: mapping.bytes.clone(),
                })
                .collect(),
        })
    }

    fn into_image(self) -> Result<MemoryCheckpointImage, ()> {
        let image = MemoryCheckpointImage {
            version: self.memory,
            address_limit: self.address_limit,
            shared_limits: SharedLimits {
                objects: self.limits[0].try_into().map_err(|_| ())?,
                object_bytes: self.limits[1].try_into().map_err(|_| ())?,
                total_bytes: self.limits[2].try_into().map_err(|_| ())?,
            },
            shared: SharedStoreSnapshot {
                generations: self.generations,
                objects: self.objects.into_iter().map(ObjectWire::into_value).collect(),
            },
            ledger: MemoryLedgerSnapshot {
                generation: self.ledger_generation,
                regions: self
                    .regions
                    .into_iter()
                    .map(RegionWire::into_value)
                    .collect::<Result<_, _>>()?,
            },
            mappings: self
                .mappings
                .into_iter()
                .map(|mapping| MemoryMappingSnapshot {
                    region: mapping.region,
                    bytes: mapping.bytes,
                })
                .collect(),
        };
        Ok(image)
    }

    fn checksum(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
        })
    }
}

impl ObjectWire {
    fn from_value(value: &SharedObjectSnapshot) -> Self {
        Self {
            slot: value.id.slot,
            generation: value.id.generation,
            owner: value.owner,
            seals: value.seals.bits(),
            bytes: value.bytes.clone(),
        }
    }

    fn into_value(self) -> SharedObjectSnapshot {
        SharedObjectSnapshot {
            id: SharedObjectId {
                slot: self.slot,
                generation: self.generation,
            },
            owner: self.owner,
            seals: SharedSeal::from_bits(self.seals),
            bytes: self.bytes,
        }
    }
}

impl RegionWire {
    fn from_value(value: Region) -> Self {
        Self {
            start: value.range().start().get(),
            length: value.range().length(),
            protection: value.protection().bits(),
            backing: BackingWire::from_value(value.backing()),
            offset: value.backing_offset(),
        }
    }

    fn into_value(self) -> Result<Region, ()> {
        let range = AddressRange::nonempty(GuestAddress::new(self.start), self.length).map_err(|_| ())?;
        Region::from_checkpoint(
            range,
            Protection::from_bits(self.protection).ok_or(())?,
            self.backing.into_value(),
            self.offset,
        )
        .map_err(|_| ())
    }
}

impl BackingWire {
    fn from_value(value: Backing) -> Self {
        match value {
            Backing::Shared(value) => Self::Shared {
                slot: value.object.slot,
                generation: value.object.generation,
                offset: value.offset,
                length: value.length,
                write_shared: value.write_shared,
            },
            Backing::Anonymous { identity, shared } => Self::Anonymous { identity, shared },
            Backing::File { identity, shared } => Self::File {
                device: identity.device,
                object: identity.object,
                shared,
            },
        }
    }

    fn into_value(self) -> Backing {
        match self {
            Self::Shared {
                slot,
                generation,
                offset,
                length,
                write_shared,
            } => Backing::Shared(SharedBackingRef {
                object: SharedObjectId { slot, generation },
                offset,
                length,
                write_shared,
            }),
            Self::Anonymous { identity, shared } => Backing::Anonymous { identity, shared },
            Self::File { device, object, shared } => Backing::File {
                identity: FileIdentity { device, object },
                shared,
            },
        }
    }
}
