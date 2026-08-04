use hl_task::*;
use serde::{Deserialize, Serialize};
use std::io::Write;
pub(in crate::checkpoint) const TASK_BYTES_MAXIMUM: usize = 4 * 1024 * 1024;
const WIRE_VERSION: u32 = 7;
struct BoundedBytes(Vec<u8>);
impl BoundedBytes {
    fn new() -> Self {
        Self(Vec::new())
    }
    fn finish(self) -> Vec<u8> {
        self.0
    }
}
impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let length = self
            .0
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("checkpoint size overflow"))?;
        if length > TASK_BYTES_MAXIMUM {
            return Err(std::io::Error::other("checkpoint size limit"));
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
pub(super) struct TaskWire {
    wire: u32,
    task: u32,
    registry: RegistryWire,
    processes: Vec<ProcessReferenceWire>,
    threads: Vec<ThreadReferenceWire>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RegistryWire {
    config: [u64; 5],
    process_generations: Vec<u16>,
    thread_generations: Vec<u16>,
    session_generations: Vec<u16>,
    group_generations: Vec<u16>,
    init: Option<IdWire>,
    processes: Vec<ProcessWire>,
    threads: Vec<ThreadWire>,
    waits: Vec<WaitWire>,
    children: Vec<ChildWire>,
    sessions: Vec<SessionWire>,
    groups: Vec<GroupWire>,
    next: [u64; 3],
    user_namespaces: Vec<UserNamespaceWire>,
    uts_namespaces: Vec<UtsWire>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UtsWire {
    id: NamespaceWire,
    owner: NamespaceWire,
    hostname: Vec<u8>,
    domainname: Vec<u8>,
}
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IdWire {
    slot: u32,
    generation: u16,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProcessWire {
    id: IdWire,
    generation: u16,
    lifecycle: u8,
    parent: Option<IdWire>,
    children: Vec<IdWire>,
    threads: Vec<IdWire>,
    leader: IdWire,
    session: IdWire,
    group: IdWire,
    terminal_detached: bool,
    child_class: u8,
    execed: bool,
    arguments: Vec<Vec<u8>>,
    name: [u8; 16],
    credentials: CredentialsWire,
    limits: Vec<LimitWire>,
    exit: Option<ExitWire>,
    signals: ProcessSignalWire,
    namespaces: NamespaceSetWire,
    parent_death_signal: u32,
    child_subreaper: bool,
    cpu_self_nanoseconds: u64,
    cpu_children_nanoseconds: u64,
    dumpable: bool,
    oom_score_adj: i16,
    timer_slack: u64,
    thp_disabled: bool,
    #[serde(default = "default_mce_policy")]
    mce_policy: u32,
    #[serde(default)]
    personality: u32,
}

const fn default_mce_policy() -> u32 {
    2
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ThreadWire {
    id: IdWire,
    generation: u16,
    process: IdWire,
    lifecycle: u8,
    cancellation_pending: bool,
    signal_pending: bool,
    signals: ThreadSignalWire,
    robust_head: Option<u64>,
    clear_tid: Option<u64>,
    name: [u8; 16],
    affinity: Option<[u64; 16]>,
    #[serde(default)]
    schedule: Option<[i64; 3]>,
    #[serde(default)]
    nice: Option<i8>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CredentialsWire {
    users: [u32; 4],
    groups: [u32; 4],
    supplementary: Vec<u32>,
    capabilities: [u64; 4],
    bounding: u64,
    secure_bits: u32,
    keep_capabilities: bool,
    no_new_privileges: bool,
    setid: [bool; 2],
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LimitWire {
    resource: u8,
    soft: u64,
    hard: u64,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExitWire {
    kind: u8,
    value: u8,
    dumped_core: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProcessSignalWire {
    actions: Vec<ActionWire>,
    pending: Vec<SignalWire>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ThreadSignalWire {
    mask: u64,
    stack: StackWire,
    pending: Vec<SignalWire>,
    deferred: u64,
    frames: Vec<[u64; 2]>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ActionWire {
    signal: u8,
    disposition: u8,
    handler: u64,
    flags: u64,
    restorer: u64,
    mask: u64,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StackWire {
    state: u8,
    pointer: u64,
    size: u64,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SignalWire {
    signal: u8,
    code: i32,
    error: i32,
    sender_process: u32,
    sender_user: u32,
    value: u64,
    address: u64,
    source_tag: u32,
}
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NamespaceWire {
    kind: u8,
    serial: u64,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NamespaceSetWire {
    uts: NamespaceWire,
    ipc: NamespaceWire,
    network: NamespaceWire,
    mount: NamespaceWire,
    user: NamespaceWire,
    pid: NamespaceWire,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UserNamespaceWire {
    id: NamespaceWire,
    parent: Option<NamespaceWire>,
    owner: u32,
    user_map: Option<Vec<[u32; 3]>>,
    group_map: Option<Vec<[u32; 3]>>,
    setgroups: u8,
    user_authority: bool,
    group_authority: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WaitWire {
    parent: IdWire,
    child: IdWire,
    status: ExitWire,
    sequence: u64,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ChildWire {
    parent: IdWire,
    child: IdWire,
    group: IdWire,
    class: u8,
    kind: u8,
    status: Option<ExitWire>,
    signal: Option<u8>,
    sequence: u64,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SessionWire {
    id: IdWire,
    leader: IdWire,
    groups: Vec<IdWire>,
    foreground: Option<IdWire>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GroupWire {
    id: IdWire,
    session: IdWire,
    leader: IdWire,
    members: Vec<IdWire>,
    orphaned: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProcessReferenceWire {
    process: IdWire,
    descriptors: Option<u64>,
    shared: Vec<u64>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ThreadReferenceWire {
    thread: IdWire,
    execution: u64,
    tls: u64,
    host: u64,
    seccomp: u64,
}
impl TaskWire {
    pub(super) fn encode(image: &TaskRegistryImage) -> Result<Vec<u8>, ()> {
        image.validate().map_err(|_| ())?;
        let wire = Self::from_image(image)?;
        let mut bytes = BoundedBytes::new();
        serde_json::to_writer(&mut bytes, &wire).map_err(|_| ())?;
        Ok(bytes.finish())
    }
    pub(super) fn decode(bytes: &[u8]) -> Result<TaskRegistryImage, ()> {
        if bytes.len() > TASK_BYTES_MAXIMUM {
            return Err(());
        }
        let wire: Self = serde_json::from_slice(bytes).map_err(|_| ())?;
        let image = wire.into_image()?;
        image.validate().map_err(|_| ())?;
        Ok(image)
    }
    fn from_image(image: &TaskRegistryImage) -> Result<Self, ()> {
        let registry = &image.registry;
        Ok(Self {
            wire: WIRE_VERSION,
            task: image.version,
            registry: RegistryWire {
                config: [
                    registry.config.max_processes.try_into().map_err(|_| ())?,
                    registry.config.max_threads.try_into().map_err(|_| ())?,
                    registry.config.max_groups.try_into().map_err(|_| ())?,
                    registry.config.max_pending_signals.try_into().map_err(|_| ())?,
                    registry.config.online_cpus.try_into().map_err(|_| ())?,
                ],
                process_generations: registry.process_generations.clone(),
                thread_generations: registry.thread_generations.clone(),
                session_generations: registry.session_generations.clone(),
                group_generations: registry.process_group_generations.clone(),
                init: registry.init.map(IdentityWire::process),
                processes: registry
                    .processes
                    .iter()
                    .map(ProcessWire::from_value)
                    .collect::<Result<_, _>>()?,
                threads: registry
                    .threads
                    .iter()
                    .map(ThreadWire::from_value)
                    .collect::<Result<_, _>>()?,
                waits: registry.wait_events.iter().map(WaitWire::from_value).collect(),
                children: registry.child_events.iter().map(ChildWire::from_value).collect(),
                sessions: registry.sessions.iter().map(SessionWire::from_value).collect(),
                groups: registry.process_groups.iter().map(GroupWire::from_value).collect(),
                next: [
                    registry.next_transaction,
                    registry.next_wait_sequence,
                    registry.next_namespace,
                ],
                user_namespaces: registry
                    .user_namespaces
                    .iter()
                    .map(UserNamespaceWire::from_value)
                    .collect(),
                uts_namespaces: registry
                    .uts_namespaces
                    .iter()
                    .map(|(id, value)| UtsWire {
                        id: NamespaceWire::from_value(*id),
                        owner: NamespaceWire::from_value(value.owner()),
                        hostname: value.hostname.clone(),
                        domainname: value.domainname.clone(),
                    })
                    .collect(),
            },
            processes: image
                .processes
                .iter()
                .map(|value| ProcessReferenceWire {
                    process: IdentityWire::process(value.process),
                    descriptors: value.descriptor_table.map(|key| key.0),
                    shared: value.shared_resources.iter().map(|key| key.0).collect(),
                })
                .collect(),
            threads: image
                .threads
                .iter()
                .map(|value| ThreadReferenceWire {
                    thread: IdentityWire::thread(value.thread),
                    execution: value.execution.0,
                    tls: value.tls.0,
                    host: value.host.0,
                    seccomp: value.seccomp.0,
                })
                .collect(),
        })
    }
    fn into_image(self) -> Result<TaskRegistryImage, ()> {
        if self.wire != WIRE_VERSION {
            return Err(());
        }
        let registry = self.registry;
        let image = TaskRegistryImage {
            version: self.task,
            registry: RegistrySnapshot {
                config: RegistryConfig {
                    max_processes: registry.config[0].try_into().map_err(|_| ())?,
                    max_threads: registry.config[1].try_into().map_err(|_| ())?,
                    max_groups: registry.config[2].try_into().map_err(|_| ())?,
                    max_pending_signals: registry.config[3].try_into().map_err(|_| ())?,
                    online_cpus: registry.config[4].try_into().map_err(|_| ())?,
                },
                process_generations: registry.process_generations,
                thread_generations: registry.thread_generations,
                session_generations: registry.session_generations,
                process_group_generations: registry.group_generations,
                init: registry.init.map(IdentityWire::process_from).transpose()?,
                processes: registry
                    .processes
                    .into_iter()
                    .map(ProcessWire::into_value)
                    .collect::<Result<_, _>>()?,
                threads: registry
                    .threads
                    .into_iter()
                    .map(ThreadWire::into_value)
                    .collect::<Result<_, _>>()?,
                wait_events: registry
                    .waits
                    .into_iter()
                    .map(WaitWire::into_value)
                    .collect::<Result<_, _>>()?,
                child_events: registry
                    .children
                    .into_iter()
                    .map(ChildWire::into_value)
                    .collect::<Result<_, _>>()?,
                sessions: registry
                    .sessions
                    .into_iter()
                    .map(SessionWire::into_value)
                    .collect::<Result<_, _>>()?,
                process_groups: registry
                    .groups
                    .into_iter()
                    .map(GroupWire::into_value)
                    .collect::<Result<_, _>>()?,
                next_transaction: registry.next[0],
                next_wait_sequence: registry.next[1],
                next_namespace: registry.next[2],
                user_namespaces: registry
                    .user_namespaces
                    .into_iter()
                    .map(UserNamespaceWire::into_value)
                    .collect::<Result<_, _>>()?,
                uts_namespaces: registry
                    .uts_namespaces
                    .into_iter()
                    .map(|value| {
                        Ok((
                            NamespaceWire::into_value(value.id)?,
                            UtsIdentity::owned(
                                value.hostname,
                                value.domainname,
                                NamespaceWire::into_value(value.owner)?,
                            )
                            .map_err(|_| ())?,
                        ))
                    })
                    .collect::<Result<_, ()>>()?,
            },
            processes: self
                .processes
                .into_iter()
                .map(|value| {
                    Ok(ProcessCheckpointReference {
                        process: IdentityWire::process_from(value.process)?,
                        descriptor_table: value.descriptors.map(TaskResourceKey),
                        shared_resources: value.shared.into_iter().map(TaskResourceKey).collect(),
                    })
                })
                .collect::<Result<_, ()>>()?,
            threads: self
                .threads
                .into_iter()
                .map(|value| {
                    Ok(ThreadCheckpointReference {
                        thread: IdentityWire::thread_from(value.thread)?,
                        execution: TaskResourceKey(value.execution),
                        tls: TaskResourceKey(value.tls),
                        host: TaskResourceKey(value.host),
                        seccomp: TaskResourceKey(value.seccomp),
                    })
                })
                .collect::<Result<_, ()>>()?,
        };
        Ok(image)
    }
}
mod identity;
mod namespace;
mod relation;
mod signal;

use identity::IdentityWire;
