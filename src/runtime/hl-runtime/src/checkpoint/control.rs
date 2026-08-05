use std::sync::{Arc, Mutex};

use hl_checkpoint::{
    CheckpointReader, CheckpointSink, CheckpointSource, CheckpointWriter, ImageError, ImageLimits, Section, SectionKind,
};

const PARTICIPANT_MAXIMUM: usize = 8;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum Role {
    Task = 0,
    Descriptors = 1,
    Memory = 2,
    Provider = 3,
    Event = 4,
    Network = 5,
    Ipc = 6,
    Execution = 7,
}

impl Role {
    fn section(self) -> SectionKind {
        SectionKind::new(u32::from(self as u8) + 1).expect("checkpoint role identifiers are nonzero")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Freeze,
    CapturePrepare,
    Snapshot,
    CapturePublish,
    Thaw,
    Validate,
    Stage,
    Commit,
    Resume,
}

#[derive(Debug)]
pub enum Error {
    InvalidParticipants,
    Image(ImageError),
    MissingSection(Role),
    UnexpectedSection(u32),
    Version { role: Role, expected: u32, actual: u32 },
    Participant { role: Role, phase: Phase },
}

impl From<ImageError> for Error {
    fn from(error: ImageError) -> Self {
        Self::Image(error)
    }
}

pub trait Participant: Send + Sync {
    fn role(&self) -> Role;
    fn version(&self) -> u32;
    fn dependencies(&self) -> &[Role];
    fn freeze(&self) -> Result<(), ()>;
    /// Begins an unpublished capture transaction while every domain is frozen.
    fn capture_prepare(&self) -> Result<(), ()> {
        Ok(())
    }
    fn snapshot(&self) -> Result<Vec<u8>, ()>;
    /// Binds unpublished external resources to the canonical whole-image digest.
    fn capture_publish(&self, _digest: [u8; 32]) -> Result<(), ()> {
        Ok(())
    }
    /// Aborts unpublished resources after any capture or sink-publication failure.
    fn capture_abort(&self) {}
    /// Releases capture transaction bookkeeping after the sink commits.
    fn capture_finish(&self) {}
    fn thaw(&self) -> Result<(), ()>;
    fn validate(&self, image: &hl_checkpoint::CheckpointImage, section: &Section) -> Result<(), ()>;
    fn stage(&self, section: &Section) -> Result<u64, ()>;
    fn stage_bound(&self, _digest: [u8; 32], section: &Section) -> Result<u64, ()> {
        self.stage(section)
    }
    fn commit(&self, reservation: u64) -> Result<(), ()>;
    fn rollback(&self, reservation: u64);
    fn resume(&self, reservation: u64) -> Result<(), ()>;
    /// Releases rollback state after every participant resumed successfully.
    fn finish(&self, _reservation: u64) {}
}

pub struct RuntimeCheckpointCoordinator {
    participants: Vec<Arc<dyn Participant>>,
    limits: ImageLimits,
    transaction: Mutex<()>,
}

struct CaptureTransaction {
    participants: Vec<Arc<dyn Participant>>,
    finished: bool,
}

struct FreezeTransaction {
    participants: Vec<Arc<dyn Participant>>,
    thawed: bool,
}

impl FreezeTransaction {
    fn new(capacity: usize) -> Self {
        Self {
            participants: Vec::with_capacity(capacity),
            thawed: false,
        }
    }

    fn freeze(&mut self, participant: &Arc<dyn Participant>) -> Result<(), ()> {
        participant.freeze()?;
        self.participants.push(participant.clone());
        Ok(())
    }

    fn thaw(mut self) -> Result<(), Error> {
        let result = Self::thaw_all(&self.participants);
        self.thawed = true;
        result
    }

    fn thaw_all(participants: &[Arc<dyn Participant>]) -> Result<(), Error> {
        let mut failure = None;
        for participant in participants.iter().rev() {
            if participant.thaw().is_err() && failure.is_none() {
                failure = Some(Error::Participant {
                    role: participant.role(),
                    phase: Phase::Thaw,
                });
            }
        }
        failure.map_or(Ok(()), Err)
    }

    fn thaw_best_effort(participants: &[Arc<dyn Participant>]) {
        for participant in participants.iter().rev() {
            match participant.thaw() {
                Ok(()) | Err(()) => {}
            }
        }
    }
}

impl Drop for FreezeTransaction {
    fn drop(&mut self) {
        if !self.thawed {
            Self::thaw_best_effort(&self.participants);
        }
    }
}

impl CaptureTransaction {
    fn new(capacity: usize) -> Self {
        Self {
            participants: Vec::with_capacity(capacity),
            finished: false,
        }
    }

    fn prepare(&mut self, participant: &Arc<dyn Participant>) -> Result<(), ()> {
        participant.capture_prepare()?;
        self.participants.push(participant.clone());
        Ok(())
    }

    fn publish(&self, digest: [u8; 32]) -> Result<(), Role> {
        for participant in &self.participants {
            participant.capture_publish(digest).map_err(|()| participant.role())?;
        }
        Ok(())
    }

    fn finish(mut self) {
        for participant in &self.participants {
            participant.capture_finish();
        }
        self.finished = true;
    }
}

impl Drop for CaptureTransaction {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        for participant in self.participants.iter().rev() {
            participant.capture_abort();
        }
    }
}

impl RuntimeCheckpointCoordinator {
    pub fn new(participants: Vec<Arc<dyn Participant>>, limits: ImageLimits) -> Result<Self, Error> {
        let roles = participants
            .iter()
            .map(|participant| participant.role())
            .collect::<Vec<_>>();
        let ordered = !participants.is_empty()
            && participants.len() <= PARTICIPANT_MAXIMUM
            && participants.windows(2).all(|pair| pair[0].role() < pair[1].role())
            && participants.iter().all(|participant| {
                participant.version() != 0
                    && participant
                        .dependencies()
                        .iter()
                        .all(|dependency| *dependency < participant.role() && roles.contains(dependency))
            });
        if !ordered {
            return Err(Error::InvalidParticipants);
        }
        Ok(Self {
            participants,
            limits,
            transaction: Mutex::new(()),
        })
    }

    #[must_use]
    pub fn roles(&self) -> Vec<Role> {
        self.participants.iter().map(|participant| participant.role()).collect()
    }

    pub fn checkpoint<S: CheckpointSink>(&self, sink: &mut S) -> Result<(), Error> {
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut frozen = FreezeTransaction::new(self.participants.len());
        for participant in &self.participants {
            if frozen.freeze(participant).is_err() {
                let error = Error::Participant {
                    role: participant.role(),
                    phase: Phase::Freeze,
                };
                frozen.thaw()?;
                return Err(error);
            }
        }
        let result = self.capture(sink);
        let thaw = frozen.thaw();
        result.and(thaw)
    }

    fn capture<S: CheckpointSink>(&self, sink: &mut S) -> Result<(), Error> {
        let mut transaction = CaptureTransaction::new(self.participants.len());
        for participant in &self.participants {
            if transaction.prepare(participant).is_err() {
                return Err(Error::Participant {
                    role: participant.role(),
                    phase: Phase::CapturePrepare,
                });
            }
        }
        let mut writer = CheckpointWriter::new(self.limits);
        for participant in &self.participants {
            let bytes = participant.snapshot().map_err(|()| Error::Participant {
                role: participant.role(),
                phase: Phase::Snapshot,
            })?;
            writer.push(Section::new(participant.role().section(), participant.version(), bytes))?;
        }
        let image = writer.prepare()?;
        transaction.publish(image.digest()).map_err(|role| Error::Participant {
            role,
            phase: Phase::CapturePublish,
        })?;
        image.publish(sink)?;
        transaction.finish();
        Ok(())
    }

    pub fn restore<S: CheckpointSource>(&self, source: &mut S) -> Result<(), Error> {
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let image = CheckpointReader::new(self.limits).read(source)?;
        self.validate_image(&image)?;
        let mut staged = Vec::with_capacity(self.participants.len());
        for participant in &self.participants {
            let section = image
                .section(participant.role().section())
                .expect("validated image contains every participant");
            match participant.stage_bound(image.digest(), section) {
                Ok(reservation) if reservation != 0 => {
                    staged.push((participant.clone(), reservation));
                }
                _ => {
                    Self::rollback(&staged);
                    return Err(Error::Participant {
                        role: participant.role(),
                        phase: Phase::Stage,
                    });
                }
            }
        }
        for (participant, reservation) in &staged {
            if participant.commit(*reservation).is_err() {
                Self::rollback(&staged);
                return Err(Error::Participant {
                    role: participant.role(),
                    phase: Phase::Commit,
                });
            }
        }
        for (participant, reservation) in &staged {
            if participant.resume(*reservation).is_err() {
                Self::rollback(&staged);
                return Err(Error::Participant {
                    role: participant.role(),
                    phase: Phase::Resume,
                });
            }
        }
        for (participant, reservation) in &staged {
            participant.finish(*reservation);
        }
        Ok(())
    }

    fn validate_image(&self, image: &hl_checkpoint::CheckpointImage) -> Result<(), Error> {
        for section in image.sections() {
            if !self.known_section(section) {
                return Err(Error::UnexpectedSection(section.kind().get()));
            }
        }
        for participant in &self.participants {
            let role = participant.role();
            let section = image.section(role.section()).ok_or(Error::MissingSection(role))?;
            if section.version() != participant.version() {
                return Err(Error::Version {
                    role,
                    expected: participant.version(),
                    actual: section.version(),
                });
            }
            participant.validate(image, section).map_err(|()| Error::Participant {
                role,
                phase: Phase::Validate,
            })?;
        }
        Ok(())
    }

    fn known_section(&self, section: &Section) -> bool {
        self.participants
            .iter()
            .any(|participant| participant.role().section() == section.kind())
    }

    fn rollback(staged: &[(Arc<dyn Participant>, u64)]) {
        for (participant, reservation) in staged.iter().rev() {
            participant.rollback(*reservation);
        }
    }
}
