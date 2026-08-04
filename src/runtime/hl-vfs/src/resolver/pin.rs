use super::{NodeHandle, ResolveError, VfsHost};

pub(super) struct PinStack<'host, H: VfsHost> {
    host: &'host H,
    pins: Vec<NodeHandle>,
}

impl<'host, H: VfsHost> PinStack<'host, H> {
    pub(super) fn root(host: &'host H) -> Result<Self, ResolveError> {
        let root = host.pin_root().map_err(ResolveError::Host)?;
        Ok(Self { host, pins: vec![root] })
    }

    pub(super) fn current(&self) -> NodeHandle {
        *self.pins.last().expect("root pin is always present")
    }

    pub(super) fn push(&mut self, pin: NodeHandle) {
        self.pins.push(pin);
    }

    pub(super) fn pop(&mut self) {
        if self.pins.len() > 1 {
            if let Some(pin) = self.pins.pop() {
                self.host.close(pin);
            }
        }
    }

    pub(super) fn reset(&mut self) -> Result<(), ResolveError> {
        while let Some(pin) = self.pins.pop() {
            self.host.close(pin);
        }
        self.pins.push(self.host.pin_root().map_err(ResolveError::Host)?);
        Ok(())
    }

    pub(super) fn truncate(&mut self, length: usize) {
        while self.pins.len() > length.max(1) {
            if let Some(pin) = self.pins.pop() {
                self.host.close(pin);
            }
        }
    }

    pub(super) fn take_current(&mut self) -> NodeHandle {
        let current = self.pins.pop().expect("root pin is always present");
        while let Some(pin) = self.pins.pop() {
            self.host.close(pin);
        }
        current
    }
}

impl<H: VfsHost> Drop for PinStack<'_, H> {
    fn drop(&mut self) {
        while let Some(pin) = self.pins.pop() {
            self.host.close(pin);
        }
    }
}
