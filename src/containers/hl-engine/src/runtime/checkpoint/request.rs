use super::{
    CLAIM, COMMIT, CapturePhase, DIGEST, GROUP_ABORT, GROUP_BEGIN, GROUP_COMMIT, GROUP_COUNT, GROUP_PRESENT,
    MutationAdmission, OBJECT_ABORT, OBJECT_BEGIN, OBJECT_FINISH, OBJECT_TELL, OBJECT_WRITE, OBJECT_WRITE_AT, Object,
    PARTICIPANT_REGISTERED, PAYLOAD_MAX, RECOVERY_COMPLETE, Reply, Request, SEAL_MEMBERSHIP, SOURCE_LIST,
    SOURCE_READ, SOURCE_SIZE,
    STATUS_ALREADY, Server, UNCLAIM,
};

impl Server {
    pub(super) fn dispatch(&self, id: u64, request: &Request, name: &str, payload: &[u8]) -> Reply {
        #[cfg(test)]
        self.dispatches.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if !self.request_in_scope(id, request, name) {
            hl_log::hl_debug!(
                hl_log::tag::CHECKPOINT,
                "checkpoint request rejected: op={} generation={} name={name:?}",
                request.op,
                request.generation
            );
            return Reply::error();
        }
        let key = (id, request.stream);
        match request.op {
            OBJECT_BEGIN => self.begin_object(key, name),
            OBJECT_WRITE | OBJECT_WRITE_AT => self.write_object(key, request, payload),
            OBJECT_TELL => self.tell_object(key),
            OBJECT_FINISH => self.finish_object(key),
            OBJECT_ABORT => self.abort_object(key),
            GROUP_BEGIN => self.begin_group(name),
            GROUP_COMMIT => self.commit_group(name),
            GROUP_ABORT => self.abort_group(name),
            CLAIM => self.local_mutation(|| self.claim(name)),
            UNCLAIM => self.unclaim(name),
            GROUP_PRESENT => self.group_present(name),
            GROUP_COUNT => self.group_count(name),
            DIGEST => self.digest(),
            COMMIT => self.commit(payload),
            SOURCE_LIST => self.source_list(name),
            SOURCE_SIZE => self.source_size(name),
            SOURCE_READ => self.source_read(name, request),
            RECOVERY_COMPLETE => self.complete_recovery(),
            PARTICIPANT_REGISTERED => self.participant_registered(request, payload),
            SEAL_MEMBERSHIP => self.seal_membership(request),
            _ => Reply::error(),
        }
    }

    /// Answers whether one host process ever proved exact membership of this capture.
    ///
    /// Read-only, and it publishes nothing, so it is dispatched like the other rendezvous queries rather
    /// than behind the `REGISTER_READY` gate: the coordinator asks it before its own registration. It is
    /// still scope-checked by `request_in_scope`, which admits it only while a capture is `Active` or
    /// `Publishing` at exactly the generation named in the request -- there is no ledger to consult
    /// outside one, and answering `0` outside one would be an exemption granted by a broker that had
    /// sealed no membership at all.
    ///
    /// Every failure is an ERROR reply rather than a `0`: the caller reads a non-OK status as "unknown",
    /// which withholds the exemption. A poisoned lock, a missing ledger, or a malformed frame must never
    /// be readable as "that process was never a member".
    fn participant_registered(&self, request: &Request, payload: &[u8]) -> Reply {
        let Ok(bytes) = <[u8; 8]>::try_from(payload) else {
            return Reply::error();
        };
        let host_pid = u64::from_ne_bytes(bytes);
        let Ok(participants) = self.participants.lock() else {
            return Reply::error();
        };
        let Some(ledger) = participants.as_ref() else {
            return Reply::error();
        };
        if host_pid == 0 {
            return Reply::error();
        }
        Reply::value(u64::from(ledger.registered(u64::from(request.generation), host_pid)))
    }

    /// Closes membership for this capture and answers the exact number of processes that proved it.
    ///
    /// The manifest's expected process set is fixed HERE and nowhere else. Enumeration in the
    /// coordinator is a point-in-time scan of a tree that forks and exits across the instant it is
    /// taken, so it can name a set the image does not have and miss one the image does; the ledger
    /// names the processes that actually registered, and sealing it stops the set moving while the
    /// coordinator counts committed groups against it.
    ///
    /// Every failure is an ERROR reply, never a count: the coordinator publishes a manifest only on an
    /// exact match, so an unreadable ledger must refuse the capture rather than be read as a number.
    fn seal_membership(&self, request: &Request) -> Reply {
        let Ok(mut participants) = self.participants.lock() else {
            return Reply::error();
        };
        let Some(ledger) = participants.as_mut() else {
            return Reply::error();
        };
        ledger
            .seal(u64::from(request.generation))
            .map_or_else(|_| Reply::error(), Reply::value)
    }

    fn begin_object(&self, key: (u64, u64), name: &str) -> Reply {
        self.local_mutation(|| {
            let Ok(mut state) = self.state.lock() else {
                return Reply::error();
            };
            let object = Object {
                name: name.into(),
                bytes: Vec::new(),
            };
            if state.open.insert(key, object).is_some() {
                Reply::error()
            } else {
                Reply::ok()
            }
        })
    }

    fn write_object(&self, key: (u64, u64), request: &Request, payload: &[u8]) -> Reply {
        self.local_mutation(|| {
            let Ok(mut state) = self.state.lock() else {
                return Reply::error();
            };
            let Some(object) = state.open.get_mut(&key) else {
                return Reply::error();
            };
            if request.op == OBJECT_WRITE {
                object.bytes.extend_from_slice(payload);
                return Reply::ok();
            }
            let Some(end) = usize::try_from(request.offset)
                .ok()
                .and_then(|offset| offset.checked_add(payload.len()))
            else {
                return Reply::error();
            };
            let offset = end - payload.len();
            object.bytes.resize(object.bytes.len().max(end), 0);
            object.bytes[offset..end].copy_from_slice(payload);
            Reply::ok()
        })
    }

    fn tell_object(&self, key: (u64, u64)) -> Reply {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.open.get(&key).map(|object| object.bytes.len() as u64))
            .map_or_else(Reply::error, Reply::value)
    }

    fn abort_object(&self, key: (u64, u64)) -> Reply {
        self.local_mutation(|| {
            if let Ok(mut state) = self.state.lock() {
                state.open.remove(&key);
            }
            Reply::ok()
        })
    }

    fn begin_group(&self, name: &str) -> Reply {
        self.local_mutation(|| {
            let Ok(mut state) = self.state.lock() else {
                return Reply::error();
            };
            if state.staged.insert(name.into(), Vec::new()).is_some() {
                Reply::error()
            } else {
                Reply::ok()
            }
        })
    }

    fn abort_group(&self, name: &str) -> Reply {
        // A native process emits GROUP_ABORT only after its process image has
        // been refused.  That is a failure of the whole process-tree image,
        // not recoverable cleanup of one member: a manifest containing every
        // other process would be authoritative but unrestorable.
        let Ok(Some(admission)) = self.mutation_admission() else {
            return Reply::error();
        };
        {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.staged.remove(name);
            state
                .open
                .retain(|_, object| object.name.split_once('/').is_none_or(|(group, _)| group != name));
        }
        if admission.finish(Err(super::CaptureFailure::Failed)).is_err() {
            self.interrupt_channels();
        }
        // The manifest may already own the irreversible publication point if
        // it entered Publishing first. Native participants synchronously send
        // GROUP_ABORT before exiting, and the coordinator joins them before
        // COMMIT, so a legitimate participant refusal always wins this race.
        Reply::error()
    }

    fn unclaim(&self, name: &str) -> Reply {
        self.local_mutation(|| {
            if let Ok(mut state) = self.state.lock() {
                state.claims.remove(name);
            }
            Reply::ok()
        })
    }

    fn group_present(&self, name: &str) -> Reply {
        self.state.lock().map_or_else(
            |_| Reply::error(),
            |state| Reply::value(u64::from(state.groups.contains(name))),
        )
    }

    fn group_count(&self, name: &str) -> Reply {
        self.state.lock().map_or_else(
            |_| Reply::error(),
            |state| Reply::value(state.groups.iter().filter(|group| group.starts_with(name)).count() as u64),
        )
    }

    fn digest(&self) -> Reply {
        // A capture asks for the digest of what it has published; a restore asks
        // for the digest of the committed image. Falling back to the stored image
        // inside a capture scope would answer the first question with the second
        // image whenever the capture has published nothing yet.
        let capturing = matches!(
            self.capture_lock().map(|capture| capture.phase),
            Ok(CapturePhase::Active { .. } | CapturePhase::Publishing { .. })
        );
        let digest = self
            .state
            .lock()
            .ok()
            .and_then(|state| (!state.digest.is_empty()).then(|| Self::image_hash(&state.digest)))
            .or_else(|| (!capturing).then(|| self.stored_digest().ok()).flatten());
        let Some((hash, files, bytes)) = digest else {
            self.fail("checkpoint digest could not be computed".into());
            return Reply::error();
        };
        let mut payload = Vec::with_capacity(24);
        payload.extend_from_slice(&hash.to_ne_bytes());
        payload.extend_from_slice(&files.to_ne_bytes());
        payload.extend_from_slice(&bytes.to_ne_bytes());
        Reply::payload(payload)
    }

    fn commit(&self, payload: &[u8]) -> Reply {
        if self.publish_manifest(payload).is_err() {
            self.fail("checkpoint store rejected manifest".into());
            Reply::error()
        } else {
            Reply::ok()
        }
    }

    fn source_size(&self, name: &str) -> Reply {
        self.source_get(name).map_or_else(
            |()| Reply::status(STATUS_ALREADY),
            |bytes| Reply::value(bytes.len() as u64),
        )
    }

    fn source_read(&self, name: &str, request: &Request) -> Reply {
        let Ok(bytes) = self.source_get(name) else {
            return Reply::error();
        };
        let Ok(offset) = usize::try_from(request.offset) else {
            return Reply::error();
        };
        if offset >= bytes.len() {
            return Reply::payload(Vec::new());
        }
        let length = usize::try_from(request.length).unwrap_or(0).min(PAYLOAD_MAX);
        Reply::payload(bytes[offset..offset.saturating_add(length).min(bytes.len())].to_vec())
    }

    fn complete_recovery(&self) -> Reply {
        let Ok(mut capture) = self.capture_lock() else {
            return Reply::error();
        };
        let CapturePhase::Recovery { id, .. } = capture.phase else {
            return Reply::error();
        };
        if capture.mutations != 0 || !capture.recovery_report_published {
            return Reply::error();
        }
        capture.phase = CapturePhase::Aborting { id };
        self.capture_changed.notify_all();
        drop(capture);
        let discarded = self.discard_transaction(std::time::Instant::now() + super::ABORT_SETTLEMENT_TIMEOUT);
        self.finish_recovery(id, discarded)
    }

    fn finish_recovery(&self, id: u64, discarded: Result<(), super::CaptureFailure>) -> Reply {
        let Ok(mut capture) = self.capture_lock() else {
            return Reply::error();
        };
        if !matches!(capture.phase, CapturePhase::Aborting { id: active } if active == id) || capture.mutations != 0 {
            return Reply::error();
        }
        capture.phase = CapturePhase::RecoveryFinished { id, result: discarded };
        self.capture_changed.notify_all();
        if discarded.is_ok() { Reply::ok() } else { Reply::error() }
    }

    fn mutation_admission(&self) -> Result<Option<MutationAdmission<'_>>, ()> {
        let admission = self.admit_mutation().map_err(|_| ())?;
        let active = matches!(
            self.capture_lock().map(|capture| capture.phase),
            Ok(CapturePhase::Active { .. })
        );
        (!active || admission.is_some()).then_some(admission).ok_or(())
    }

    fn local_mutation(&self, operation: impl FnOnce() -> Reply) -> Reply {
        let Ok(admission) = self.mutation_admission() else {
            return Reply::error();
        };
        let reply = operation();
        if admission.is_some_and(|admission| admission.finish(Ok(())).is_err()) {
            Reply::error()
        } else {
            reply
        }
    }

    fn stage_object(&self, object: Object) -> Result<(), Object> {
        let Some(group) = object.name.split_once('/').map(|(group, _)| group.to_owned()) else {
            return Err(object);
        };
        let Ok(mut state) = self.state.lock() else {
            return Err(object);
        };
        let Some(staged) = state.staged.get_mut(&group) else {
            return Err(object);
        };
        staged.push(object);
        Ok(())
    }

    fn finish_recovery_report(&self) -> Reply {
        let Ok(mut capture) = self.capture_lock() else {
            return Reply::error();
        };
        if !matches!(capture.phase, CapturePhase::Recovery { .. }) || capture.mutations != 0 {
            return Reply::error();
        }
        capture.recovery_report_published = true;
        Reply::ok()
    }

    fn finish_object(&self, key: (u64, u64)) -> Reply {
        let Ok(admission) = self.mutation_admission() else {
            return Reply::error();
        };
        let Some(object) = self.state.lock().ok().and_then(|mut state| state.open.remove(&key)) else {
            return Reply::error();
        };
        let object = match self.stage_object(object) {
            Ok(()) => {
                return admission
                    .map_or(Ok(()), |admission| admission.finish(Ok(())))
                    .map_or_else(|_| Reply::error(), |()| Reply::ok());
            }
            Err(object) => object,
        };
        let recovery_report = object.name == "RECOVERY.jsonl";
        if self.publish(&object, admission).is_err() {
            self.fail(format!("checkpoint store rejected {}", object.name));
            return Reply::error();
        }
        if recovery_report {
            return self.finish_recovery_report();
        }
        Reply::ok()
    }

    fn commit_group(&self, name: &str) -> Reply {
        let Ok(admission) = self.mutation_admission() else {
            return Reply::error();
        };
        let objects = self
            .state
            .lock()
            .ok()
            .and_then(|mut state| state.staged.remove(name))
            .unwrap_or_default();
        let deadline = admission.as_ref().map(|admission| admission.deadline);
        let mut result = Ok(());
        for object in &objects {
            if let Err(error) = self.publish_object(object, deadline) {
                result = Err(Self::publication_failure(error));
                self.fail(format!("checkpoint store rejected {}", object.name));
                break;
            }
        }
        if result.is_ok()
            && let Ok(mut state) = self.state.lock()
        {
            state.groups.insert(name.into());
        }
        if admission.is_some_and(|admission| admission.finish(result).is_err()) || result.is_err() {
            Reply::error()
        } else {
            Reply::ok()
        }
    }

    pub(super) fn claim(&self, name: &str) -> Reply {
        let Ok(mut state) = self.state.lock() else {
            return Reply::error();
        };
        if state.claims.insert(name.into()) {
            Reply::ok()
        } else {
            Reply::status(STATUS_ALREADY)
        }
    }

    pub(super) fn source_list(&self, prefix: &str) -> Reply {
        let names = self.source_deadline().map_err(|_| ()).and_then(|deadline| {
            deadline
                .map_or_else(|| self.source.list(), |deadline| self.source.list_until(deadline))
                .map_err(|_| ())
        });
        let Ok(names) = names else {
            return Reply::error();
        };
        let mut seen = Vec::new();
        for full in names {
            let entry = full.split_once('/').map_or(full.as_str(), |(head, _)| head);
            if entry.starts_with(prefix) && !seen.iter().any(|held| held == entry) {
                seen.push(entry.to_owned());
            }
        }
        let mut payload = Vec::new();
        for entry in &seen {
            payload.extend_from_slice(entry.as_bytes());
            payload.push(0);
        }
        Reply::counted_payload(seen.len() as u64, payload)
    }
}
