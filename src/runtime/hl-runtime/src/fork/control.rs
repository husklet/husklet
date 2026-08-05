use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use hl_task::{FORK_WIRE_VERSION, ForkRequest, ForkWireSnapshot, MAX_FORK_PARTICIPANTS};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum ParticipantRole {
    Task = 0,
    Descriptors = 1,
    Memory = 2,
    Provider = 3,
    Execution = 4,
    Network = 5,
    Event = 6,
    Ipc = 7,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Phase {
    Prepare = 0,
    Freeze = 1,
    CloneParent = 2,
    CloneChild = 3,
    RepairParent = 4,
    RepairChild = 5,
    Commit = 6,
    Rollback = 7,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Context {
    pub transaction: u64,
    pub request: ForkRequest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Event {
    pub role: ParticipantRole,
    pub phase: Phase,
    pub reservation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Outcome {
    pub context: Context,
    pub transcript: Vec<Event>,
    pub wire: ForkWireSnapshot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidRequest,
    InvalidParticipants,
    Cancelled,
    Participant { role: ParticipantRole, phase: Phase },
}

pub trait Cancellation: Send + Sync {
    fn cancelled(&self) -> bool;
}

pub trait Participant: Send + Sync {
    fn role(&self) -> ParticipantRole;
    fn prepare(&self, context: Context) -> Result<u64, ()>;
    fn freeze(&self, context: Context, reservation: u64) -> Result<(), ()>;
    fn clone_parent(&self, context: Context, reservation: u64) -> Result<(), ()>;
    fn clone_child(&self, context: Context, reservation: u64) -> Result<(), ()>;
    fn clone_with_artifacts(&self, context: Context, reservation: u64, _: &ArtifactExchange) -> Result<(), ()> {
        self.clone_child(context, reservation)
    }
    fn repair_parent(&self, context: Context, reservation: u64) -> Result<(), ()>;
    fn repair_child(&self, context: Context, reservation: u64) -> Result<(), ()>;
    fn commit(&self, context: Context, reservation: u64) -> Result<(), ()>;
    fn rollback(&self, context: Context, reservation: u64);
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ArtifactKey {
    transaction: u64,
    role: ParticipantRole,
    reservation: u64,
    artifact: TypeId,
}

/// Transaction-local, typed capabilities exchanged between ordered fork participants.
///
/// Values are qualified by both the transaction and the publishing participant's
/// reservation, so a stale participant cannot observe a capability from a reused
/// transaction-local slot.
#[derive(Default)]
pub struct ArtifactExchange {
    values: Mutex<HashMap<ArtifactKey, Arc<dyn Any + Send + Sync>>>,
}

impl ArtifactExchange {
    pub fn publish<T: Any + Send + Sync>(
        &self,
        context: Context,
        role: ParticipantRole,
        reservation: u64,
        value: Arc<T>,
    ) -> Result<(), ()> {
        if context.transaction == 0 || reservation == 0 {
            return Err(());
        }
        let key = ArtifactKey {
            transaction: context.transaction,
            role,
            reservation,
            artifact: TypeId::of::<T>(),
        };
        let mut values = self.values.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if values.keys().any(|existing| {
            existing.transaction == context.transaction
                && existing.role == role
                && existing.artifact == TypeId::of::<T>()
        }) {
            return Err(());
        }
        values.insert(key, value);
        Ok(())
    }

    pub fn get<T: Any + Send + Sync>(&self, context: Context, role: ParticipantRole) -> Option<Arc<T>> {
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|(key, _)| {
                key.transaction == context.transaction && key.role == role && key.artifact == TypeId::of::<T>()
            })
            .map(|(_, value)| value)
            .cloned()?
            .downcast()
            .ok()
    }

    pub fn take<T: Any + Send + Sync>(&self, context: Context, role: ParticipantRole) -> Option<Arc<T>> {
        let mut values = self.values.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = values
            .keys()
            .find(|key| key.transaction == context.transaction && key.role == role && key.artifact == TypeId::of::<T>())
            .copied()?;
        values.remove(&key)?.downcast().ok()
    }

    fn remove_transaction(&self, transaction: u64) {
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|key, _| key.transaction != transaction);
    }
}

pub struct Coordinator {
    participants: Vec<Arc<dyn Participant>>,
    artifacts: ArtifactExchange,
    transaction: Mutex<()>,
    next_transaction: AtomicU64,
}

impl Coordinator {
    pub fn new(participants: Vec<Arc<dyn Participant>>) -> Result<Self, Error> {
        if participants.len() != MAX_FORK_PARTICIPANTS
            || participants
                .iter()
                .enumerate()
                .any(|(index, participant)| participant.role() as usize != index)
        {
            return Err(Error::InvalidParticipants);
        }
        Ok(Self {
            participants,
            artifacts: ArtifactExchange::default(),
            transaction: Mutex::new(()),
            next_transaction: AtomicU64::new(1),
        })
    }

    pub fn fork(&self, request: ForkRequest, cancellation: &dyn Cancellation) -> Result<Outcome, Error> {
        request.validate().map_err(|_| Error::InvalidRequest)?;
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let transaction = self.next_transaction.fetch_add(1, Ordering::Relaxed);
        if transaction == 0 {
            return Err(Error::InvalidRequest);
        }
        let context = Context { transaction, request };
        let mut reservations = Vec::with_capacity(self.participants.len());
        let mut transcript = Vec::new();
        for participant in &self.participants {
            if cancellation.cancelled() {
                self.rollback(context, &reservations, &mut transcript);
                self.artifacts.remove_transaction(transaction);
                return Err(Error::Cancelled);
            }
            match participant.prepare(context) {
                Ok(reservation) if reservation != 0 => {
                    reservations.push((participant.clone(), reservation));
                    transcript.push(Event {
                        role: participant.role(),
                        phase: Phase::Prepare,
                        reservation,
                    });
                }
                _ => {
                    let error = Error::Participant {
                        role: participant.role(),
                        phase: Phase::Prepare,
                    };
                    self.rollback(context, &reservations, &mut transcript);
                    self.artifacts.remove_transaction(transaction);
                    return Err(error);
                }
            }
        }
        for phase in [
            Phase::Freeze,
            Phase::CloneParent,
            Phase::CloneChild,
            Phase::RepairParent,
            Phase::RepairChild,
            Phase::Commit,
        ] {
            if let Err(error) = self.run_phase(context, phase, &reservations, &mut transcript, cancellation) {
                self.rollback(context, &reservations, &mut transcript);
                self.artifacts.remove_transaction(transaction);
                return Err(error);
            }
        }
        let wire = ForkWireSnapshot {
            version: FORK_WIRE_VERSION,
            transaction,
            request,
            phase: Phase::Commit as u8,
            participant_reservations: reservations
                .iter()
                .map(|(participant, reservation)| (participant.role() as u8, *reservation))
                .collect(),
        };
        if wire.validate().is_err() {
            self.artifacts.remove_transaction(transaction);
            return Err(Error::InvalidRequest);
        }
        self.artifacts.remove_transaction(transaction);
        Ok(Outcome {
            context,
            transcript,
            wire,
        })
    }

    fn run_phase(
        &self,
        context: Context,
        phase: Phase,
        reservations: &[(Arc<dyn Participant>, u64)],
        transcript: &mut Vec<Event>,
        cancellation: &dyn Cancellation,
    ) -> Result<(), Error> {
        let forward = reservations.iter();
        let reverse = reservations.iter().rev();
        let participants: Box<dyn Iterator<Item = &(Arc<dyn Participant>, u64)>> = if phase == Phase::Commit {
            Box::new(reverse)
        } else {
            Box::new(forward)
        };
        for (participant, reservation) in participants {
            if cancellation.cancelled() {
                return Err(Error::Cancelled);
            }
            let result = match phase {
                Phase::Freeze => participant.freeze(context, *reservation),
                Phase::CloneParent => participant.clone_parent(context, *reservation),
                Phase::CloneChild => participant.clone_with_artifacts(context, *reservation, &self.artifacts),
                Phase::RepairParent => participant.repair_parent(context, *reservation),
                Phase::RepairChild => participant.repair_child(context, *reservation),
                Phase::Commit => participant.commit(context, *reservation),
                Phase::Prepare | Phase::Rollback => Err(()),
            };
            if result.is_err() {
                return Err(Error::Participant {
                    role: participant.role(),
                    phase,
                });
            }
            transcript.push(Event {
                role: participant.role(),
                phase,
                reservation: *reservation,
            });
        }
        Ok(())
    }

    fn rollback(&self, context: Context, reservations: &[(Arc<dyn Participant>, u64)], transcript: &mut Vec<Event>) {
        for (participant, reservation) in reservations.iter().rev() {
            participant.rollback(context, *reservation);
            transcript.push(Event {
                role: participant.role(),
                phase: Phase::Rollback,
                reservation: *reservation,
            });
        }
    }
}
