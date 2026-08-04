use super::*;

pub(super) struct IdentityWire;
impl IdentityWire {
    pub(super) fn process(value: ProcessId) -> IdWire {
        let (slot, generation) = value.wire_parts();
        IdWire { slot, generation }
    }
    pub(super) fn thread(value: ThreadId) -> IdWire {
        let (slot, generation) = value.wire_parts();
        IdWire { slot, generation }
    }
    pub(super) fn session(value: SessionId) -> IdWire {
        let (slot, generation) = value.wire_parts();
        IdWire { slot, generation }
    }
    pub(super) fn group(value: ProcessGroupId) -> IdWire {
        let (slot, generation) = value.wire_parts();
        IdWire { slot, generation }
    }
    pub(super) fn process_from(value: IdWire) -> Result<ProcessId, ()> {
        ProcessId::from_wire(value.slot, value.generation).ok_or(())
    }
    pub(super) fn thread_from(value: IdWire) -> Result<ThreadId, ()> {
        ThreadId::from_wire(value.slot, value.generation).ok_or(())
    }
    pub(super) fn session_from(value: IdWire) -> Result<SessionId, ()> {
        SessionId::from_wire(value.slot, value.generation).ok_or(())
    }
    pub(super) fn group_from(value: IdWire) -> Result<ProcessGroupId, ()> {
        ProcessGroupId::from_wire(value.slot, value.generation).ok_or(())
    }
}
impl ProcessWire {
    pub(super) fn from_value(value: &ProcessSnapshot) -> Result<Self, ()> {
        Ok(Self {
            id: IdentityWire::process(value.id),
            generation: value.generation,
            lifecycle: match value.lifecycle {
                ProcessLifecycle::Starting => 1,
                ProcessLifecycle::Running => 2,
                ProcessLifecycle::Stopped => 3,
                ProcessLifecycle::Exiting => 4,
                ProcessLifecycle::Zombie => 5,
            },
            parent: value.parent.map(IdentityWire::process),
            children: value.children.iter().copied().map(IdentityWire::process).collect(),
            threads: value.threads.iter().copied().map(IdentityWire::thread).collect(),
            leader: IdentityWire::thread(value.leader),
            session: IdentityWire::session(value.session),
            group: IdentityWire::group(value.process_group),
            child_class: match value.child_class {
                ChildClass::Standard => 1,
                ChildClass::Clone => 2,
            },
            execed: value.execed,
            arguments: value.arguments.clone(),
            name: value.name,
            credentials: CredentialsWire::from_value(&value.credentials),
            limits: value.limits.iter().map(LimitWire::from_value).collect(),
            exit: value.exit_status.map(ExitWire::from_value),
            signals: ProcessSignalWire::from_value(&value.signals),
            namespaces: NamespaceSetWire::from_value(value.namespaces),
            parent_death_signal: value.parent_death_signal,
            child_subreaper: value.child_subreaper,
            cpu_self_nanoseconds: value.cpu_usage.self_nanoseconds,
            cpu_children_nanoseconds: value.cpu_usage.children_nanoseconds,
            dumpable: value.dumpable,
            oom_score_adj: value.oom_score_adj,
            timer_slack: value.timer_slack,
            thp_disabled: value.thp_disabled,
            mce_policy: value.mce_policy,
            personality: value.personality,
        })
    }
    pub(super) fn into_value(self) -> Result<ProcessSnapshot, ()> {
        Ok(ProcessSnapshot {
            id: IdentityWire::process_from(self.id)?,
            generation: self.generation,
            lifecycle: match self.lifecycle {
                1 => ProcessLifecycle::Starting,
                2 => ProcessLifecycle::Running,
                3 => ProcessLifecycle::Stopped,
                4 => ProcessLifecycle::Exiting,
                5 => ProcessLifecycle::Zombie,
                _ => return Err(()),
            },
            parent: self.parent.map(IdentityWire::process_from).transpose()?,
            children: self
                .children
                .into_iter()
                .map(IdentityWire::process_from)
                .collect::<Result<_, _>>()?,
            threads: self
                .threads
                .into_iter()
                .map(IdentityWire::thread_from)
                .collect::<Result<_, _>>()?,
            leader: IdentityWire::thread_from(self.leader)?,
            session: IdentityWire::session_from(self.session)?,
            process_group: IdentityWire::group_from(self.group)?,
            child_class: match self.child_class {
                1 => ChildClass::Standard,
                2 => ChildClass::Clone,
                _ => return Err(()),
            },
            execed: self.execed,
            arguments: self.arguments,
            name: self.name,
            credentials: self.credentials.into_value()?,
            limits: self
                .limits
                .into_iter()
                .map(LimitWire::into_value)
                .collect::<Result<_, _>>()?,
            exit_status: self.exit.map(ExitWire::into_value).transpose()?,
            signals: self.signals.into_value()?,
            namespaces: self.namespaces.into_value()?,
            parent_death_signal: self.parent_death_signal,
            child_subreaper: self.child_subreaper,
            cpu_usage: hl_task::CpuUsage {
                self_nanoseconds: self.cpu_self_nanoseconds,
                children_nanoseconds: self.cpu_children_nanoseconds,
            },
            dumpable: self.dumpable,
            oom_score_adj: self.oom_score_adj,
            timer_slack: self.timer_slack,
            thp_disabled: self.thp_disabled,
            mce_policy: self.mce_policy,
            personality: self.personality,
        })
    }
}
impl ThreadWire {
    pub(super) fn from_value(value: &ThreadSnapshot) -> Result<Self, ()> {
        Ok(Self {
            id: IdentityWire::thread(value.id),
            generation: value.generation,
            process: IdentityWire::process(value.process),
            lifecycle: match value.lifecycle {
                ThreadLifecycle::Starting => 1,
                ThreadLifecycle::Runnable => 2,
                ThreadLifecycle::Blocked => 3,
                ThreadLifecycle::Exiting => 4,
            },
            cancellation_pending: value.cancellation_pending,
            signal_pending: value.signal_pending,
            signals: ThreadSignalWire::from_value(&value.signals),
            robust_head: value.robust_list.map(|item| item.head),
            clear_tid: value.clear_tid,
            name: value.name,
            affinity: value.affinity.map(CpuAffinity::words),
            schedule: Some([
                i64::from(value.schedule.policy()),
                i64::from(value.schedule.priority()),
                value.schedule.resets_on_fork() as i64,
            ]),
            nice: Some(value.schedule.nice()),
        })
    }
    pub(super) fn into_value(self) -> Result<ThreadSnapshot, ()> {
        Ok(ThreadSnapshot {
            id: IdentityWire::thread_from(self.id)?,
            generation: self.generation,
            process: IdentityWire::process_from(self.process)?,
            lifecycle: match self.lifecycle {
                1 => ThreadLifecycle::Starting,
                2 => ThreadLifecycle::Runnable,
                3 => ThreadLifecycle::Blocked,
                4 => ThreadLifecycle::Exiting,
                _ => return Err(()),
            },
            cancellation_pending: self.cancellation_pending,
            signal_pending: self.signal_pending,
            signals: self.signals.into_value()?,
            robust_list: self.robust_head.map(RobustListRegistration::new),
            clear_tid: self.clear_tid,
            name: self.name,
            affinity: match self.affinity {
                Some(words) => Some(CpuAffinity::from_words(words).ok_or(())?),
                None => None,
            },
            schedule: match self.schedule {
                Some([policy, priority, reset]) => hl_task::SchedulingProfile::restore(
                    u32::try_from(policy).map_err(|_| ())?,
                    i32::try_from(priority).map_err(|_| ())?,
                    reset != 0,
                )
                .ok_or(())?
                .with_nice(i32::from(self.nice.unwrap_or(0))),
                None => hl_task::SchedulingProfile::OTHER,
            },
        })
    }
}
impl CredentialsWire {
    pub(super) fn from_value(value: &ProcessCredentials) -> Self {
        Self {
            users: [
                value.real_user,
                value.effective_user,
                value.saved_user,
                value.filesystem_user,
            ],
            groups: [
                value.real_group,
                value.effective_group,
                value.saved_group,
                value.filesystem_group,
            ],
            supplementary: value.supplementary_groups().to_vec(),
            capabilities: [
                value.capabilities.effective,
                value.capabilities.permitted,
                value.capabilities.inheritable,
                value.capabilities.ambient,
            ],
            bounding: value.capability_bounding,
            secure_bits: value.secure_bits,
            keep_capabilities: value.keep_capabilities,
            no_new_privileges: value.no_new_privileges,
            setid: [value.setid_permitted, value.setid_effective],
        }
    }
    pub(super) fn into_value(self) -> Result<ProcessCredentials, ()> {
        let mut value = ProcessCredentials::new(
            self.users[0],
            self.groups[0],
            &self.supplementary,
            self.supplementary.len(),
        )
        .map_err(|_| ())?;
        value.real_user = self.users[0];
        value.effective_user = self.users[1];
        value.saved_user = self.users[2];
        value.filesystem_user = self.users[3];
        value.real_group = self.groups[0];
        value.effective_group = self.groups[1];
        value.saved_group = self.groups[2];
        value.filesystem_group = self.groups[3];
        value.capabilities = CapabilitySets {
            effective: self.capabilities[0],
            permitted: self.capabilities[1],
            inheritable: self.capabilities[2],
            ambient: self.capabilities[3],
        };
        value.capability_bounding = self.bounding;
        value.secure_bits = self.secure_bits;
        value.keep_capabilities = self.keep_capabilities;
        value.no_new_privileges = self.no_new_privileges;
        value.setid_permitted = self.setid[0];
        value.setid_effective = self.setid[1];
        Ok(value)
    }
}
impl LimitWire {
    pub(super) fn from_value((resource, limit): &(Resource, Limit)) -> Self {
        Self {
            resource: Self::code(*resource),
            soft: limit.soft,
            hard: limit.hard,
        }
    }
    pub(super) fn into_value(self) -> Result<(Resource, Limit), ()> {
        Ok((
            Self::resource(self.resource)?,
            Limit::new(self.soft, self.hard).map_err(|_| ())?,
        ))
    }
    pub(super) fn code(value: Resource) -> u8 {
        match value {
            Resource::CpuTime => 1,
            Resource::FileSize => 2,
            Resource::Data => 3,
            Resource::Stack => 4,
            Resource::Core => 5,
            Resource::ResidentSet => 6,
            Resource::Processes => 7,
            Resource::OpenFiles => 8,
            Resource::LockedMemory => 9,
            Resource::AddressSpace => 10,
            Resource::Locks => 11,
            Resource::PendingSignals => 12,
            Resource::MessageQueue => 13,
            Resource::Nice => 14,
            Resource::RealtimePriority => 15,
            Resource::RealtimeTime => 16,
        }
    }
    pub(super) fn resource(value: u8) -> Result<Resource, ()> {
        Ok(match value {
            1 => Resource::CpuTime,
            2 => Resource::FileSize,
            3 => Resource::Data,
            4 => Resource::Stack,
            5 => Resource::Core,
            6 => Resource::ResidentSet,
            7 => Resource::Processes,
            8 => Resource::OpenFiles,
            9 => Resource::LockedMemory,
            10 => Resource::AddressSpace,
            11 => Resource::Locks,
            12 => Resource::PendingSignals,
            13 => Resource::MessageQueue,
            14 => Resource::Nice,
            15 => Resource::RealtimePriority,
            16 => Resource::RealtimeTime,
            _ => return Err(()),
        })
    }
}
impl ExitWire {
    pub(super) fn from_value(value: ExitStatus) -> Self {
        match value {
            ExitStatus::Code(code) => Self {
                kind: 1,
                value: code,
                dumped_core: false,
            },
            ExitStatus::Signal { signal, dumped_core } => Self {
                kind: 2,
                value: signal,
                dumped_core,
            },
        }
    }
    pub(super) fn into_value(self) -> Result<ExitStatus, ()> {
        match self.kind {
            1 if !self.dumped_core => Ok(ExitStatus::Code(self.value)),
            2 => Ok(ExitStatus::Signal {
                signal: self.value,
                dumped_core: self.dumped_core,
            }),
            _ => Err(()),
        }
    }
}
