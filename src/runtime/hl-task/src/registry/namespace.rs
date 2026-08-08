use super::TaskRegistry;
use crate::{NamespaceId, NamespaceKind, NamespaceSet, ProcessId, TaskError, UserNamespace};

impl TaskRegistry {
    pub(super) fn initial_users() -> std::collections::BTreeMap<NamespaceId, UserNamespace> {
        let id = NamespaceSet::initial().user;
        std::collections::BTreeMap::from([(
            id,
            UserNamespace {
                id,
                parent: None,
                owner: 0,
            },
        )])
    }

    pub fn namespaces(&self, process: ProcessId) -> Result<NamespaceSet, TaskError> {
        Ok(Self::process(&self.lock(), process)?.namespaces)
    }

    pub fn uts_identity(&self, process: ProcessId) -> Result<crate::UtsIdentity, TaskError> {
        let state = self.lock();
        let id = Self::process(&state, process)?.namespaces.uts;
        state.uts_namespaces.get(&id).cloned().ok_or(TaskError::InvalidSnapshot)
    }

    pub fn uts_namespace(&self, identifier: NamespaceId) -> Result<crate::UtsIdentity, TaskError> {
        self.lock()
            .uts_namespaces
            .get(&identifier)
            .cloned()
            .ok_or(TaskError::InvalidSnapshot)
    }

    pub fn replace_uts_namespace(
        &self,
        actor: ProcessId,
        thread: crate::ThreadId,
        identifier: NamespaceId,
        hostname: Option<Vec<u8>>,
        domainname: Option<Vec<u8>>,
    ) -> Result<(), TaskError> {
        let mut state = self.lock();
        if Self::thread(&state, thread)?.process != actor {
            return Err(TaskError::WrongProcess);
        }
        let process = Self::process(&state, actor)?;
        let current_user = process.namespaces.user;
        if !process.credentials.has_capability(crate::CapabilitySets::SYS_ADMIN) {
            return Err(TaskError::PermissionDenied(crate::Denial::Capability(
                crate::CapabilitySets::SYS_ADMIN,
            )));
        }
        let existing = state
            .uts_namespaces
            .get(&identifier)
            .cloned()
            .ok_or(TaskError::InvalidSnapshot)?;
        let mut owner = Some(existing.owner());
        let mut visible = false;
        while let Some(namespace) = owner {
            if namespace == current_user {
                visible = true;
                break;
            }
            owner = state.user_namespaces.get(&namespace).and_then(|value| value.parent);
        }
        if !visible {
            return Err(TaskError::PermissionDenied(crate::Denial::NamespaceNotVisible(
                existing.owner(),
            )));
        }
        let existing_owner = existing.owner();
        let replacement = crate::UtsIdentity::owned(
            hostname.unwrap_or(existing.hostname),
            domainname.unwrap_or(existing.domainname),
            existing_owner,
        )?;
        state.uts_namespaces.insert(identifier, replacement);
        Ok(())
    }

    pub fn replace_uts_identity(&self, process: ProcessId, identity: crate::UtsIdentity) -> Result<(), TaskError> {
        let mut state = self.lock();
        Self::ensure_process_unreserved(&state, process)?;
        let id = Self::process(&state, process)?.namespaces.uts;
        *state.uts_namespaces.get_mut(&id).ok_or(TaskError::InvalidSnapshot)? = identity;
        Ok(())
    }

    pub fn may_administer_uts(&self, process: ProcessId) -> Result<bool, TaskError> {
        let state = self.lock();
        let process = Self::process(&state, process)?;
        let Some(uts) = state.uts_namespaces.get(&process.namespaces.uts) else {
            return Err(TaskError::InvalidSnapshot);
        };
        let mut owner = Some(uts.owner());
        let mut visible = false;
        while let Some(namespace) = owner {
            if namespace == process.namespaces.user {
                visible = true;
                break;
            }
            owner = state.user_namespaces.get(&namespace).and_then(|value| value.parent);
        }
        // The launch root owns the initial container UTS identity even though the
        // externally reported Docker capability mask deliberately omits
        // CAP_SYS_ADMIN.  Keep that authority scoped to the owning user
        // namespace; a root identity does not gain authority over a sibling or
        // otherwise invisible UTS namespace.
        let namespace_owner = state
            .user_namespaces
            .get(&process.namespaces.user)
            .is_some_and(|namespace| namespace.owner == process.credentials.effective_user);
        Ok(visible && (process.credentials.has_capability(crate::CapabilitySets::SYS_ADMIN) || namespace_owner))
    }

    pub fn unshare_namespace(&self, process: ProcessId, kind: NamespaceKind) -> Result<NamespaceId, TaskError> {
        let mut state = self.lock();
        Self::ensure_process_unreserved(&state, process)?;
        if matches!(kind, NamespaceKind::User | NamespaceKind::Pid)
            && Self::process(&state, process)?.threads.len() != 1
        {
            return Err(TaskError::InvalidLifecycle);
        }
        let identifier = NamespaceId {
            kind,
            serial: state.next_namespace,
        };
        state.next_namespace = state.next_namespace.checked_add(1).ok_or(TaskError::InvalidCapacity)?;
        if kind == NamespaceKind::Uts {
            let source = Self::process(&state, process)?.namespaces.uts;
            let source = state.uts_namespaces.get(&source).ok_or(TaskError::InvalidSnapshot)?;
            let owner = Self::process(&state, process)?.namespaces.user;
            let identity = crate::UtsIdentity::owned(source.hostname.clone(), source.domainname.clone(), owner)?;
            state.uts_namespaces.insert(identifier, identity);
        }
        Self::set_namespace(&mut state, process, identifier)?;
        Ok(identifier)
    }

    #[cfg(test)]
    pub(crate) fn user_namespace(&self, process: ProcessId) -> Result<UserNamespace, TaskError> {
        let state = self.lock();
        let identifier = Self::process(&state, process)?.namespaces.user;
        state
            .user_namespaces
            .get(&identifier)
            .cloned()
            .ok_or(TaskError::InvalidSnapshot)
    }

    pub fn join_namespace(&self, process: ProcessId, identifier: NamespaceId) -> Result<(), TaskError> {
        if identifier.serial == 0 {
            return Err(TaskError::InvalidSnapshot);
        }
        if identifier.kind == NamespaceKind::User {
            return Err(TaskError::InvalidLifecycle);
        }
        let mut state = self.lock();
        Self::ensure_process_unreserved(&state, process)?;
        if matches!(identifier.kind, NamespaceKind::User | NamespaceKind::Pid)
            && Self::process(&state, process)?.threads.len() != 1
        {
            return Err(TaskError::InvalidLifecycle);
        }
        state.next_namespace = state
            .next_namespace
            .max(identifier.serial.checked_add(1).ok_or(TaskError::InvalidCapacity)?);
        if identifier.kind == NamespaceKind::Uts && !state.uts_namespaces.contains_key(&identifier) {
            return Err(TaskError::InvalidSnapshot);
        }
        Self::set_namespace(&mut state, process, identifier)
    }

    fn set_namespace(state: &mut super::State, process: ProcessId, identifier: NamespaceId) -> Result<(), TaskError> {
        Self::process_mut(state, process)?.namespaces.replace(identifier);
        Ok(())
    }
}
