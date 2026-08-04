use hl_ipc::{IpcResourceKey, TaskCheckpoint};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reference {
    process: u32,
    resource: u64,
}

impl Reference {
    pub(super) fn from_value(value: TaskCheckpoint) -> Self {
        Self {
            process: value.process,
            resource: value.resource.get(),
        }
    }

    pub(super) fn into_value(self) -> Result<TaskCheckpoint, ()> {
        Ok(TaskCheckpoint {
            process: self.process,
            resource: IpcResourceKey::new(self.resource).ok_or(())?,
        })
    }
}
