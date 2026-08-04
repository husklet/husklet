use super::{Handle, HandleKind, HandleNamespace, MAX_CAPACITY, NamespaceError, RemoteId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub handles: usize,
}

impl Limits {
    pub fn new(handles: usize) -> Result<Self, NamespaceError> {
        if handles == 0 || handles > MAX_CAPACITY {
            return Err(NamespaceError::InvalidCapacity);
        }
        Ok(Self { handles })
    }
}

#[must_use = "a provider handle reservation must be published or rolled back"]
pub struct HandleReservation<'namespace> {
    pub(super) namespace: &'namespace HandleNamespace,
    pub(super) handle: Handle,
    pub(super) kind: HandleKind,
    pub(super) admission: Option<crate::checkpoint_activity::ActivityAdmission>,
}

impl HandleReservation<'_> {
    pub fn publish(mut self, remote: RemoteId) -> Result<Handle, NamespaceError> {
        self.namespace.publish(self.handle, self.kind, remote)?;
        self.admission.take();
        Ok(self.handle)
    }
}

impl Drop for HandleReservation<'_> {
    fn drop(&mut self) {
        if self.admission.is_some() {
            self.namespace.cancel(self.handle);
        }
    }
}
