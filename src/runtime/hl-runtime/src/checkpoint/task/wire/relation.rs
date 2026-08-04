use super::*;

impl WaitWire {
    pub(super) fn from_value(value: &WaitEvent) -> Self {
        Self {
            parent: IdentityWire::process(value.parent),
            child: IdentityWire::process(value.child),
            status: ExitWire::from_value(value.status),
            sequence: value.sequence,
        }
    }
    pub(super) fn into_value(self) -> Result<WaitEvent, ()> {
        Ok(WaitEvent {
            parent: IdentityWire::process_from(self.parent)?,
            child: IdentityWire::process_from(self.child)?,
            status: self.status.into_value()?,
            sequence: self.sequence,
        })
    }
}
impl ChildWire {
    pub(super) fn from_value(value: &ChildEvent) -> Self {
        let (kind, status, signal) = match value.kind {
            ChildEventKind::Exited(status) => (1, Some(ExitWire::from_value(status)), None),
            ChildEventKind::Stopped(signal) => (2, None, Some(signal.get())),
            ChildEventKind::Continued => (3, None, None),
        };
        Self {
            parent: IdentityWire::process(value.parent),
            child: IdentityWire::process(value.child),
            group: IdentityWire::group(value.process_group),
            class: match value.class {
                ChildClass::Standard => 1,
                ChildClass::Clone => 2,
            },
            kind,
            status,
            signal,
            sequence: value.sequence,
        }
    }
    pub(super) fn into_value(self) -> Result<ChildEvent, ()> {
        let kind = match self.kind {
            1 if self.signal.is_none() => ChildEventKind::Exited(self.status.ok_or(())?.into_value()?),
            2 if self.status.is_none() => {
                ChildEventKind::Stopped(SignalNumber::new(self.signal.ok_or(())?).map_err(|_| ())?)
            }
            3 if self.status.is_none() && self.signal.is_none() => ChildEventKind::Continued,
            _ => return Err(()),
        };
        Ok(ChildEvent {
            parent: IdentityWire::process_from(self.parent)?,
            child: IdentityWire::process_from(self.child)?,
            process_group: IdentityWire::group_from(self.group)?,
            class: match self.class {
                1 => ChildClass::Standard,
                2 => ChildClass::Clone,
                _ => return Err(()),
            },
            kind,
            sequence: self.sequence,
        })
    }
}
impl SessionWire {
    pub(super) fn from_value(value: &SessionSnapshot) -> Self {
        Self {
            id: IdentityWire::session(value.id),
            leader: IdentityWire::process(value.leader),
            groups: value.process_groups.iter().copied().map(IdentityWire::group).collect(),
            foreground: value.foreground_group.map(IdentityWire::group),
        }
    }
    pub(super) fn into_value(self) -> Result<SessionSnapshot, ()> {
        Ok(SessionSnapshot {
            id: IdentityWire::session_from(self.id)?,
            leader: IdentityWire::process_from(self.leader)?,
            process_groups: self
                .groups
                .into_iter()
                .map(IdentityWire::group_from)
                .collect::<Result<_, _>>()?,
            foreground_group: self.foreground.map(IdentityWire::group_from).transpose()?,
        })
    }
}
impl GroupWire {
    pub(super) fn from_value(value: &ProcessGroupSnapshot) -> Self {
        Self {
            id: IdentityWire::group(value.id),
            session: IdentityWire::session(value.session),
            leader: IdentityWire::process(value.leader),
            members: value.members.iter().copied().map(IdentityWire::process).collect(),
            orphaned: value.orphaned,
        }
    }
    pub(super) fn into_value(self) -> Result<ProcessGroupSnapshot, ()> {
        Ok(ProcessGroupSnapshot {
            id: IdentityWire::group_from(self.id)?,
            session: IdentityWire::session_from(self.session)?,
            leader: IdentityWire::process_from(self.leader)?,
            members: self
                .members
                .into_iter()
                .map(IdentityWire::process_from)
                .collect::<Result<_, _>>()?,
            orphaned: self.orphaned,
        })
    }
}
