use std::collections::BTreeSet;

use hl_linux::{BpfInstruction, SeccompMode, SeccompPolicy, SeccompPolicyImage};
use hl_task::ThreadId;
use serde::{Deserialize, Serialize};

use crate::SeccompPolicySnapshot;

const VERSION: u32 = 1;
const BYTES_MAXIMUM: usize = 4 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ImageWire {
    version: u32,
    policies: Vec<Entry>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    thread: [u32; 2],
    mode: u8,
    nnp: bool,
    filters: Vec<Vec<[u32; 3]>>,
}

pub(super) struct Wire;

impl Wire {
    pub(super) fn encode(snapshot: &SeccompPolicySnapshot) -> Result<Vec<u8>, ()> {
        let policies = snapshot.policies.iter().map(Entry::from_policy).collect();
        let bytes = serde_json::to_vec(&ImageWire {
            version: VERSION,
            policies,
        })
        .map_err(|_| ())?;
        if bytes.len() > BYTES_MAXIMUM {
            Err(())
        } else {
            Ok(bytes)
        }
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<SeccompPolicySnapshot, ()> {
        if bytes.len() > BYTES_MAXIMUM {
            return Err(());
        }
        let wire: ImageWire = serde_json::from_slice(bytes).map_err(|_| ())?;
        if wire.version != VERSION {
            return Err(());
        }
        let mut seen = BTreeSet::new();
        let mut policies = Vec::with_capacity(wire.policies.len());
        for entry in wire.policies {
            let (thread, policy) = entry.into_policy()?;
            if !seen.insert(thread) {
                return Err(());
            }
            policies.push((thread, policy));
        }
        Ok(SeccompPolicySnapshot { policies })
    }
}

impl Entry {
    fn from_policy((thread, policy): &(ThreadId, SeccompPolicy)) -> Self {
        let (slot, generation) = thread.wire_parts();
        let image = policy.checkpoint_image();
        Self {
            thread: [slot, u32::from(generation)],
            mode: match image.mode {
                SeccompMode::Disabled => 0,
                SeccompMode::Strict => 1,
                SeccompMode::Filter => 2,
            },
            nnp: image.no_new_privileges,
            filters: image
                .filters
                .iter()
                .map(|filter| filter.iter().map(Self::encode_instruction).collect())
                .collect(),
        }
    }

    fn into_policy(self) -> Result<(ThreadId, SeccompPolicy), ()> {
        let generation = u16::try_from(self.thread[1]).map_err(|_| ())?;
        let thread = ThreadId::from_wire(self.thread[0], generation).ok_or(())?;
        let mode = match self.mode {
            0 => SeccompMode::Disabled,
            1 => SeccompMode::Strict,
            2 => SeccompMode::Filter,
            _ => return Err(()),
        };
        let filters = self
            .filters
            .into_iter()
            .map(|filter| filter.into_iter().map(Self::decode_instruction).collect())
            .collect::<Result<Vec<_>, ()>>()?;
        let policy = SeccompPolicy::restore_checkpoint(&SeccompPolicyImage {
            mode,
            no_new_privileges: self.nnp,
            filters,
        })
        .map_err(|_| ())?;
        Ok((thread, policy))
    }

    fn encode_instruction(instruction: &BpfInstruction) -> [u32; 3] {
        [
            u32::from(instruction.code),
            u32::from(instruction.jump_true) | u32::from(instruction.jump_false) << 8,
            instruction.value,
        ]
    }

    fn decode_instruction(instruction: [u32; 3]) -> Result<BpfInstruction, ()> {
        if instruction[0] > u32::from(u16::MAX) || instruction[1] > u32::from(u16::MAX) {
            return Err(());
        }
        Ok(BpfInstruction {
            code: instruction[0] as u16,
            jump_true: instruction[1] as u8,
            jump_false: (instruction[1] >> 8) as u8,
            value: instruction[2],
        })
    }
}
