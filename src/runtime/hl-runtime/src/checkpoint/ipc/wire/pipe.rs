use hl_ipc::{IpcPipeId, IpcResourceKey, PipeCheckpoint, PipeSnapshot};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Image {
    slot: u32,
    generation: u32,
    bytes: Vec<u8>,
    #[serde(default)]
    head_fragment: u64,
    packets: Vec<u64>,
    packet_mode: bool,
    capacity: u64,
    readers: u64,
    writers: u64,
    read_nonblocking: bool,
    write_nonblocking: bool,
    reader: u64,
    writer: u64,
}

impl Image {
    pub(super) fn from_value(value: &PipeCheckpoint) -> Result<Self, ()> {
        Ok(Self {
            slot: value.id.slot,
            generation: value.id.generation,
            bytes: value.snapshot.bytes.clone(),
            head_fragment: value.snapshot.head_fragment.try_into().map_err(|_| ())?,
            packets: value
                .snapshot
                .packets
                .iter()
                .map(|length| u64::try_from(*length).map_err(|_| ()))
                .collect::<Result<_, _>>()?,
            packet_mode: value.snapshot.packet_mode,
            capacity: value.snapshot.capacity.try_into().map_err(|_| ())?,
            readers: value.snapshot.readers.try_into().map_err(|_| ())?,
            writers: value.snapshot.writers.try_into().map_err(|_| ())?,
            read_nonblocking: value.snapshot.read_nonblocking,
            write_nonblocking: value.snapshot.write_nonblocking,
            reader: value.reader.get(),
            writer: value.writer.get(),
        })
    }

    pub(super) fn into_value(self) -> Result<PipeCheckpoint, ()> {
        Ok(PipeCheckpoint {
            id: IpcPipeId {
                slot: self.slot,
                generation: self.generation,
            },
            snapshot: PipeSnapshot {
                bytes: self.bytes,
                head_fragment: self.head_fragment.try_into().map_err(|_| ())?,
                packets: self
                    .packets
                    .into_iter()
                    .map(|length| usize::try_from(length).map_err(|_| ()))
                    .collect::<Result<_, _>>()?,
                packet_mode: self.packet_mode,
                capacity: self.capacity.try_into().map_err(|_| ())?,
                readers: self.readers.try_into().map_err(|_| ())?,
                writers: self.writers.try_into().map_err(|_| ())?,
                read_nonblocking: self.read_nonblocking,
                write_nonblocking: self.write_nonblocking,
            },
            reader: IpcResourceKey::new(self.reader).ok_or(())?,
            writer: IpcResourceKey::new(self.writer).ok_or(())?,
        })
    }
}
