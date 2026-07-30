use super::*;
impl HlState {
    pub fn set_native_frames(&mut self, frames: NativeFrames) {
        hl_log::hl_log!(
            hl_log::tag::COMPOSITOR,
            hl_log::Level::Debug,
            "native ingress installed id={}",
            frames.id()
        );
        self.native = Some(NativeState::new(frames));
    }

    pub(in crate::adapter::smithay::state) fn native_frames_enabled(&self) -> bool {
        self.native.is_some()
    }

    pub(in crate::adapter::smithay::state) fn cancel_native_commit(&mut self, surface: SurfaceId) {
        let deferred = self
            .native
            .as_mut()
            .map(|native| native.cancel(surface))
            .unwrap_or_default();
        for deferred in deferred {
            self.discard_deferred(deferred);
        }
    }

    pub(in crate::adapter::smithay::state) fn replace_native_commit(&mut self, surface: SurfaceId) {
        let deferred = self
            .native
            .as_mut()
            .map(|native| native.replace(surface))
            .unwrap_or_default();
        for deferred in deferred {
            self.discard_deferred(deferred);
        }
    }

    pub(in crate::adapter::smithay::state) fn register_native_token(&mut self, token: u64) -> bool {
        let Some(token) = NonZeroU64::new(token) else {
            return false;
        };
        self.native
            .as_mut()
            .is_some_and(|native| native.register(token))
    }

    pub(in crate::adapter::smithay::state) fn unregister_native_token(&mut self, token: u64) {
        let Some(token) = NonZeroU64::new(token) else {
            return;
        };
        if let Some(native) = self.native.as_mut() {
            native.unregister(token);
        }
    }

    pub(in crate::adapter::smithay::state) fn destroy_native_buffer(
        &mut self,
        token: u64,
        external: &WeakDmabuf,
    ) {
        let Some(token) = NonZeroU64::new(token) else {
            return;
        };
        if let Some(native) = self.native.as_mut() {
            native.settle_destroyed(token, external);
            native.unregister(token);
        }
    }

    pub(in crate::adapter::smithay::state) fn defer_native_commit(
        &mut self,
        token: u64,
        serial: u64,
        deferred: Deferred,
    ) -> Defer {
        let (Some(token), Some(serial), Some(native)) = (
            NonZeroU64::new(token),
            NonZeroU64::new(serial),
            self.native.as_mut(),
        ) else {
            self.discard_deferred(deferred);
            return Defer::Waiting;
        };
        let (result, discarded) = native.defer(Key { token, serial }, deferred);
        for evicted in discarded {
            self.discard_deferred(evicted);
        }
        result
    }

    pub(in crate::adapter::smithay::state) fn defer_external_commit(
        &mut self,
        token: u64,
        serial: u64,
        deferred: Deferred,
    ) -> Defer {
        let (Some(token), Some(native)) = (NonZeroU64::new(token), self.native.as_mut()) else {
            self.discard_deferred(deferred);
            return Defer::Waiting;
        };
        let serial = NonZeroU64::new(serial);
        let (result, discarded) = match serial {
            Some(serial) if !native.external_is_stale(token, serial) => {
                native.defer(Key { token, serial }, deferred)
            }
            Some(_) | None => native.defer_pending(token, deferred),
        };
        for evicted in discarded {
            self.discard_deferred(evicted);
        }
        result
    }

    pub fn drain_native_frames(&mut self) {
        loop {
            let cancellation = self
                .native
                .as_ref()
                .map(NativeState::take_cancellation)
                .transpose();
            match cancellation {
                Ok(Some(Some(cancellation))) => {
                    loop {
                        let prior = self.native.as_ref().and_then(|native| {
                            native.take_before(cancellation.token, cancellation.serial)
                        });
                        let Some(frame) = prior else {
                            break;
                        };
                        self.ingest_native(frame);
                    }
                    let deferred = self
                        .native
                        .as_mut()
                        .map(|native| {
                            native.cancel_key(Key {
                                token: cancellation.token,
                                serial: cancellation.serial,
                            })
                        })
                        .unwrap_or_default();
                    for deferred in deferred {
                        self.discard_deferred(deferred);
                    }
                    continue;
                }
                Ok(Some(None)) | Ok(None) => {}
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
            }
            let frame = match self.native.as_ref().map(NativeState::take_ingress) {
                Some(Ok(Some(frame))) => frame,
                Some(Ok(None)) | None => break,
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                    hl_log::hl_log!(
                        hl_log::tag::COMPOSITOR,
                        hl_log::Level::Debug,
                        "native ingress disconnected"
                    );
                    let deferred = self
                        .native
                        .take()
                        .map(|mut native| native.disconnect())
                        .unwrap_or_default();
                    for deferred in deferred {
                        self.discard_deferred(deferred);
                    }
                    self.engine.presenter_mut().discard_native();
                    break;
                }
                Some(Err(std::sync::mpsc::TryRecvError::Empty)) => break,
            };
            if self.native.is_none() {
                frame.complete(NativeFrameOutcome::Discarded);
                continue;
            }
            self.ingest_native(frame);
        }
    }

    fn ingest_native(&mut self, frame: NativeFrame) {
        let key = Key {
            token: frame.token,
            serial: frame.serial,
        };
        let surface = self
            .native
            .as_ref()
            .and_then(|native| native.waiting_surface(key));
        let root = surface.and_then(|surface| self.engine.scene.window_root(surface));
        hl_log::hl_log!(
            hl_log::tag::COMPOSITOR,
            hl_log::Level::Debug,
            "native_callback phase=guest_frame root={} surface={} token={} serial={} now_ns={} associated={}",
            root.map(|root| root.0).unwrap_or_default(),
            surface.map(|surface| surface.0).unwrap_or_default(),
            key.token,
            key.serial,
            self.engine.clock().now_nanos(),
            surface.is_some()
        );
        let (ready, discarded) = self
            .native
            .as_mut()
            .map(|native| {
                let ready = native.ingest(frame);
                (ready, native.take_discarded())
            })
            .unwrap_or_default();
        for deferred in discarded {
            self.discard_deferred(deferred);
        }
        if let Some(ready) = ready {
            self.finish_native(ready);
        }
    }

    pub(in crate::adapter::smithay::state) fn retire_native_token(&mut self, token: u64) {
        let Some(token) = NonZeroU64::new(token) else {
            return;
        };
        let deferred = self
            .native
            .as_mut()
            .map(|native| native.retire(token))
            .unwrap_or_default();
        for deferred in deferred {
            self.discard_deferred(deferred);
        }
    }

    fn discard_deferred(&mut self, deferred: Deferred) {
        if let Some(buffer) = deferred.buffer {
            buffer.release();
        }
        settle_callbacks(
            deferred.frame_callbacks,
            self.engine.clock().now_nanos(),
            |callback, time_ms| callback.done(time_ms),
        );
        for feedback in deferred.feedbacks {
            feedback.discarded();
        }
    }

    pub(in crate::adapter::smithay::state) fn finish_native(&mut self, ready: Ready) {
        let Ready {
            frame,
            mut deferred,
        } = ready;
        let metadata_source = if deferred.metadata.is_some() {
            "dmabuf"
        } else {
            "iosurface"
        };
        let metadata = deferred
            .metadata
            .or_else(|| Metadata::from_surface(&frame.surface));
        let Some(metadata) = metadata else {
            let (width, height, stride) = frame.surface.dimensions();
            hl_log::hl_warn!(
                hl_log::tag::PRESENT,
                "native import failed reason=metadata_conversion surface={} token={} serial={} metadata_source={} actual_iosurface={} actual_width={} actual_height={} actual_stride={}",
                deferred.surface.0,
                frame.token,
                frame.serial,
                metadata_source,
                frame.surface.id(),
                width,
                height,
                stride
            );
            frame.complete(NativeFrameOutcome::ImportFailed);
            self.discard_deferred(deferred);
            return;
        };
        let actual = frame.surface.dimensions();
        if let Some(reason) = metadata.failure(actual) {
            hl_log::hl_warn!(
                hl_log::tag::PRESENT,
                "native import failed reason={:?} surface={} token={} serial={} metadata_source={} expected_width={} expected_height={} expected_stride={} expected_format={:?} actual_iosurface={} actual_width={} actual_height={} actual_stride={} actual_format=bgra8",
                reason,
                deferred.surface.0,
                frame.token,
                frame.serial,
                metadata_source,
                metadata.width,
                metadata.height,
                metadata.stride,
                metadata.format,
                frame.surface.id(),
                actual.0,
                actual.1,
                actual.2
            );
            frame.complete(NativeFrameOutcome::ImportFailed);
            self.discard_deferred(deferred);
            return;
        }
        let buffer_scale = match deferred.commit.buffer {
            BufferChange::New(buffer) => buffer.buffer_scale,
            BufferChange::Removed | BufferChange::Keep => 1,
        };
        deferred.commit.buffer = BufferChange::New(BufferState {
            tex_w: metadata.width,
            tex_h: metadata.height,
            format: metadata.format,
            buffer_scale,
            gpu: true,
        });
        if let Err(frame) = self.engine.presenter_mut().attach_native(
            deferred.surface,
            frame,
            deferred.buffer.take(),
        ) {
            frame.complete(NativeFrameOutcome::ImportFailed);
            self.discard_deferred(deferred);
            return;
        }
        self.pending_callbacks
            .entry(deferred.surface)
            .or_default()
            .extend(std::mem::take(&mut deferred.frame_callbacks));
        if !deferred.feedbacks.is_empty() {
            self.pending_presentation
                .entry(deferred.surface)
                .or_default()
                .extend(std::mem::take(&mut deferred.feedbacks));
        }
        self.finish_commit(
            deferred.surface,
            deferred.commit,
            deferred.was_mapped,
            deferred.min_size,
            deferred.max_size,
        );
    }

    pub(in crate::adapter::smithay::state) fn reuse_native(&mut self, mut deferred: Deferred) {
        if let Some(buffer) = deferred.buffer.take() {
            buffer.release();
        }
        deferred.commit.buffer = BufferChange::Keep;
        self.pending_callbacks
            .entry(deferred.surface)
            .or_default()
            .extend(std::mem::take(&mut deferred.frame_callbacks));
        if !deferred.feedbacks.is_empty() {
            self.pending_presentation
                .entry(deferred.surface)
                .or_default()
                .extend(std::mem::take(&mut deferred.feedbacks));
        }
        self.finish_commit(
            deferred.surface,
            deferred.commit,
            deferred.was_mapped,
            deferred.min_size,
            deferred.max_size,
        );
    }
}
