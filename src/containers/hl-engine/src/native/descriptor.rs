use super::HostError;
use std::sync::{Arc, Mutex};

pub trait DescriptorSyscalls: Send + Sync {
    fn duplicate_cloexec(&self, descriptor: i32, minimum: i32) -> Result<i32, HostError>;
    fn close_descriptor(&self, descriptor: i32);
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DescriptorIdentity {
    slot: u16,
    generation: u32,
}

pub struct Descriptor<S: DescriptorSyscalls> {
    pub(super) syscalls: Arc<S>,
    raw: Option<i32>,
}

impl<S: DescriptorSyscalls> Descriptor<S> {
    pub(crate) fn from_raw(syscalls: Arc<S>, raw: i32) -> Result<Self, HostError> {
        (raw >= 0)
            .then_some(Self {
                syscalls,
                raw: Some(raw),
            })
            .ok_or(HostError::Invalid)
    }

    pub(crate) fn raw(&self) -> i32 {
        self.raw.expect("native descriptor is live")
    }

    pub(crate) fn syscalls(&self) -> &S {
        &self.syscalls
    }
}

impl<S: DescriptorSyscalls> Drop for Descriptor<S> {
    fn drop(&mut self) {
        if let Some(raw) = self.raw.take() {
            self.syscalls.close_descriptor(raw);
        }
    }
}

struct Slot<S: DescriptorSyscalls> {
    generation: u32,
    descriptor: Option<Descriptor<S>>,
    inherit: bool,
}

pub struct PrivateDescriptorAllocator<S: DescriptorSyscalls> {
    syscalls: Arc<S>,
    minimum: i32,
    slots: Mutex<Vec<Slot<S>>>,
}

impl<S: DescriptorSyscalls> PrivateDescriptorAllocator<S> {
    pub fn new(syscalls: Arc<S>, minimum: i32, capacity: u16) -> Result<Self, HostError> {
        if minimum < 3 || capacity == 0 {
            return Err(HostError::Invalid);
        }
        let slots = (0..capacity)
            .map(|_| Slot {
                generation: 0,
                descriptor: None,
                inherit: false,
            })
            .collect();
        Ok(Self {
            syscalls,
            minimum,
            slots: Mutex::new(slots),
        })
    }

    pub fn adopt(&self, source: &Descriptor<S>) -> Result<DescriptorIdentity, HostError> {
        let mut slots = self.slots.lock().map_err(|_| HostError::Failed)?;
        let (index, slot) = slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.descriptor.is_none())
            .ok_or(HostError::Exhausted)?;
        let raw = self.syscalls.duplicate_cloexec(source.raw(), self.minimum)?;
        let descriptor = Descriptor::from_raw(Arc::clone(&self.syscalls), raw)?;
        slot.generation = slot.generation.wrapping_add(1).max(1);
        slot.descriptor = Some(descriptor);
        slot.inherit = false;
        Ok(DescriptorIdentity {
            slot: index as u16,
            generation: slot.generation,
        })
    }

    pub fn set_inherit(&self, identity: DescriptorIdentity, inherit: bool) -> Result<(), HostError> {
        let mut slots = self.slots.lock().map_err(|_| HostError::Failed)?;
        let slot = Self::lookup(&mut slots, identity)?;
        slot.inherit = inherit;
        Ok(())
    }

    pub fn release(&self, identity: DescriptorIdentity) -> Result<(), HostError> {
        let mut slots = self.slots.lock().map_err(|_| HostError::Failed)?;
        Self::lookup(&mut slots, identity)?.descriptor.take();
        Ok(())
    }

    pub fn exec_sweep(&self) -> Result<usize, HostError> {
        let mut slots = self.slots.lock().map_err(|_| HostError::Failed)?;
        let mut released = 0;
        for slot in &mut *slots {
            if slot.descriptor.is_some() && !slot.inherit {
                slot.descriptor.take();
                released += 1;
            }
        }
        Ok(released)
    }

    fn lookup(slots: &mut [Slot<S>], identity: DescriptorIdentity) -> Result<&mut Slot<S>, HostError> {
        slots
            .get(usize::from(identity.slot))
            .filter(|slot| slot.generation == identity.generation && slot.descriptor.is_some())
            .ok_or(HostError::Invalid)?;
        slots.get_mut(usize::from(identity.slot)).ok_or(HostError::Invalid)
    }
}

pub struct ReceivedDescriptors<S: DescriptorSyscalls> {
    descriptors: Vec<Descriptor<S>>,
}

impl<S: DescriptorSyscalls> ReceivedDescriptors<S> {
    pub(crate) fn new(descriptors: Vec<Descriptor<S>>) -> Self {
        Self { descriptors }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.descriptors.len()
    }

    pub fn install<I: DescriptorInstall<S>>(mut self, installer: &mut I) -> Result<(), HostError> {
        installer.begin(self.descriptors.len())?;
        while !self.descriptors.is_empty() {
            let descriptor = self.descriptors.remove(0);
            if let Err(error) = installer.install(descriptor) {
                installer.rollback();
                return Err(error);
            }
        }
        installer.commit()
    }
}

pub trait DescriptorInstall<S: DescriptorSyscalls> {
    fn begin(&mut self, count: usize) -> Result<(), HostError>;
    fn install(&mut self, descriptor: Descriptor<S>) -> Result<(), HostError>;
    fn commit(&mut self) -> Result<(), HostError>;
    fn rollback(&mut self);
}
