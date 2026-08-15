use super::{
    BTreeMap, COMMIT, CaptureFailure, CapturePhase, DIGEST, HASH_BASIS, HASH_PRIME, MutationAdmission, OBJECT_ABORT,
    OBJECT_BEGIN, OBJECT_FINISH, OBJECT_TELL, OBJECT_WRITE, OBJECT_WRITE_AT, Object, Ordering, RECOVERY_COMPLETE,
    Request, SOURCE_LIST, SOURCE_READ, SOURCE_SIZE, Server,
};

impl Server {
    pub(super) fn publication_failure(error: crate::composition::CompositionError) -> CaptureFailure {
        match error {
            crate::composition::CompositionError::DeadlineExceeded => CaptureFailure::Deadline,
            crate::composition::CompositionError::TransactionBusy => CaptureFailure::Busy,
            _ => CaptureFailure::Failed,
        }
    }

    pub(super) fn hash_extend(mut hash: u64, bytes: &[u8]) -> u64 {
        for byte in bytes {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(HASH_PRIME);
        }
        hash
    }

    pub(super) fn included(name: &str) -> bool {
        name != "MANIFEST" && name != "RECOVERY.jsonl" && !name.starts_with(".RECOVERY.jsonl.tmp.")
    }

    pub(super) fn object_hash(name: &str, bytes: &[u8]) -> u64 {
        let mut hash = Self::hash_extend(HASH_BASIS, name.as_bytes());
        hash = Self::hash_extend(hash, &[0]);
        hash = Self::hash_extend(hash, &(bytes.len() as u64).to_ne_bytes());
        Self::hash_extend(hash, bytes)
    }

    pub(super) fn image_hash(objects: &BTreeMap<String, (u64, u64)>) -> (u64, u64, u64) {
        let mut hash = HASH_BASIS;
        let mut bytes = 0;
        for (name, (object, size)) in objects {
            hash = Self::hash_extend(hash, name.as_bytes());
            hash = Self::hash_extend(hash, &[0]);
            hash = Self::hash_extend(hash, &object.to_ne_bytes());
            bytes += size;
        }
        (hash, objects.len() as u64, bytes)
    }

    pub(super) fn publish(&self, object: &Object, admission: Option<MutationAdmission<'_>>) -> Result<(), ()> {
        if let Some(admission) = admission {
            let deadline = admission.deadline;
            let result = self
                .publish_object(object, Some(deadline))
                .map_err(Self::publication_failure)
                .and_then(|()| {
                    (std::time::Instant::now() < deadline)
                        .then_some(())
                        .ok_or(CaptureFailure::Deadline)
                });
            admission.finish(result).map_err(|_| ())?;
        } else {
            self.publish_object(object, None).map_err(|_| ())?;
        }
        Ok(())
    }

    pub(super) fn publish_object(
        &self,
        object: &Object,
        deadline: Option<std::time::Instant>,
    ) -> Result<(), crate::composition::CompositionError> {
        let transaction = self
            .transaction_token()
            .map_err(|_| crate::composition::CompositionError::RuntimeConstruction)?;
        let deadline = deadline.ok_or(crate::composition::CompositionError::RuntimeConstruction)?;
        self.sink
            .put_until(transaction, &object.name, &object.bytes, deadline)?;
        if Self::included(&object.name) {
            let mut state = self
                .state
                .lock()
                .map_err(|_| crate::composition::CompositionError::RuntimeConstruction)?;
            state.digest.insert(
                object.name.clone(),
                (
                    Self::object_hash(&object.name, &object.bytes),
                    object.bytes.len() as u64,
                ),
            );
        }
        Ok(())
    }

    pub(super) fn stored_digest(&self) -> Result<(u64, u64, u64), ()> {
        let mut objects = BTreeMap::new();
        let deadline = self.source_deadline().map_err(|_| ())?;
        let names = deadline.map_or_else(|| self.source.list(), |deadline| self.source.list_until(deadline));
        for name in names.map_err(|_| ())? {
            if Self::included(&name) {
                let bytes = deadline
                    .map_or_else(
                        || self.source.get(&name),
                        |deadline| self.source.get_until(&name, deadline),
                    )
                    .map_err(|_| ())?;
                objects.insert(name.clone(), (Self::object_hash(&name, &bytes), bytes.len() as u64));
            }
        }
        Ok(Self::image_hash(&objects))
    }

    pub(super) fn publish_manifest(&self, manifest: &[u8]) -> Result<(), CaptureFailure> {
        let (id, deadline) = {
            let mut capture = self.capture_lock()?;
            loop {
                let CapturePhase::Active { id, deadline } = capture.phase else {
                    return Err(match capture.phase {
                        CapturePhase::Finished { result: Err(error), .. } => error,
                        CapturePhase::Poisoned => CaptureFailure::Poisoned,
                        _ => CaptureFailure::Busy,
                    });
                };
                if std::time::Instant::now() >= deadline {
                    capture.phase = CapturePhase::Finished {
                        id,
                        result: Err(CaptureFailure::Deadline),
                    };
                    self.capture_changed.notify_all();
                    drop(capture);
                    self.interrupt_channels();
                    return Err(CaptureFailure::Deadline);
                }
                if capture.mutations == 0 {
                    capture.phase = CapturePhase::Publishing { id };
                    break (id, deadline);
                }
                let wait = deadline.saturating_duration_since(std::time::Instant::now());
                let (next, _) = self
                    .capture_changed
                    .wait_timeout(capture, wait)
                    .map_err(|_| CaptureFailure::Poisoned)?;
                capture = next;
            }
        };

        let transaction = self.transaction_token()?;
        let result = match self.sink.commit_until(transaction, manifest, deadline) {
            Ok(()) => Ok(()),
            Err(crate::composition::CompositionError::PublishedNotDurable) => {
                hl_log::hl_error!(
                    hl_log::tag::CHECKPOINT,
                    "checkpoint generation published but directory durability is uncertain"
                );
                Ok(())
            }
            Err(crate::composition::CompositionError::DeadlineExceeded) => Err(CaptureFailure::Deadline),
            Err(_) => Err(CaptureFailure::Failed),
        };
        let mut capture = self.capture_lock()?;
        if !matches!(capture.phase, CapturePhase::Publishing { id: active } if active == id) {
            capture.phase = CapturePhase::Poisoned;
            self.capture_changed.notify_all();
            return Err(CaptureFailure::Poisoned);
        }
        capture.phase = CapturePhase::Finished { id, result };
        if result.is_ok() {
            self.committed.store(true, Ordering::Release);
            if let Ok(mut active) = self.transaction.lock()
                && *active == Some(transaction)
            {
                *active = None;
            }
        }
        self.capture_changed.notify_all();
        result
    }

    pub(super) fn source_get(&self, name: &str) -> Result<Vec<u8>, ()> {
        self.source_deadline()
            .map_err(|_| ())?
            .map_or_else(
                || self.source.get(name),
                |deadline| self.source.get_until(name, deadline),
            )
            .map_err(|_| ())
    }

    pub(super) fn recovery_object_request(&self, connection: u64, request: &Request, name: &str) -> bool {
        match request.op {
            OBJECT_BEGIN => name == "RECOVERY.jsonl",
            OBJECT_WRITE | OBJECT_WRITE_AT | OBJECT_TELL | OBJECT_FINISH | OBJECT_ABORT => self
                .state
                .lock()
                .ok()
                .and_then(|state| {
                    state
                        .open
                        .get(&(connection, request.stream))
                        .map(|object| object.name.as_str() == "RECOVERY.jsonl")
                })
                .unwrap_or(false),
            _ => false,
        }
    }

    pub(super) fn request_in_scope(&self, connection: u64, request: &Request, name: &str) -> bool {
        let Ok(capture) = self.capture_lock() else { return false };
        match capture.phase {
            CapturePhase::Idle => {
                request.generation == 0 && matches!(request.op, SOURCE_LIST | SOURCE_SIZE | SOURCE_READ | DIGEST)
            }
            CapturePhase::Recovery { id, deadline } => {
                let bound_restore_connection = request.generation == 0
                    && self
                        .recovery_connections
                        .lock()
                        .ok()
                        .and_then(|connections| connections.get(&connection).copied())
                        == Some(id);
                (u64::from(request.generation) == id || bound_restore_connection)
                    && std::time::Instant::now() < deadline
                    && (matches!(request.op, SOURCE_LIST | SOURCE_SIZE | SOURCE_READ | DIGEST)
                        || request.op == RECOVERY_COMPLETE
                        || self.recovery_object_request(connection, request, name))
            }
            CapturePhase::Complete | CapturePhase::Aborting { .. } => false,
            CapturePhase::Active { id, .. } | CapturePhase::Publishing { id } => u64::from(request.generation) == id,
            CapturePhase::Finished { id, .. } => u64::from(request.generation) == id && request.op == COMMIT,
            CapturePhase::Poisoned => false,
        }
    }
}
