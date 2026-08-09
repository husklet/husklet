use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use hl_descriptor::{
    DescriptionIdentity, ObjectError, ObjectKind, OfdMetadata, OfdTimestamp, OpenFileDescription, OperationActor,
    OperationCancellation, OperationContext, Readiness, ReadinessObserver, ReadinessSubscription, StatusFlags,
};

use crate::{Catalog, CatalogError, ForegroundGroup, Pair, PairId, ReadError, Signal};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Endpoint {
    Master,
    Slave,
}

/// Consumer-owned terminal signal delivery boundary.
pub trait SignalSink: Send + Sync {
    fn publish(
        &self,
        actor: Option<OperationActor>,
        terminal: PairId,
        foreground: Option<ForegroundGroup>,
        signal: Signal,
    );
}

/// One terminal endpoint with ordinary open-file-description lifetime.
pub struct Description {
    pair: Arc<Pair>,
    endpoint: Endpoint,
    catalog: Weak<Catalog>,
    binding: Mutex<Option<(DescriptionIdentity, Weak<Bindings>)>>,
    signals: Arc<dyn SignalSink>,
    nonblocking: AtomicBool,
    closed: AtomicBool,
}

impl Description {
    #[must_use]
    pub fn new(pair: Arc<Pair>, endpoint: Endpoint, catalog: Weak<Catalog>, signals: Arc<dyn SignalSink>) -> Self {
        Self::with_status(pair, endpoint, catalog, signals, StatusFlags::default())
    }

    #[must_use]
    pub fn with_status(
        pair: Arc<Pair>,
        endpoint: Endpoint,
        catalog: Weak<Catalog>,
        signals: Arc<dyn SignalSink>,
        status: StatusFlags,
    ) -> Self {
        pair.open_endpoint(endpoint);
        Self {
            pair,
            endpoint,
            catalog,
            binding: Mutex::new(None),
            signals,
            nonblocking: AtomicBool::new(status.bits() & StatusFlags::NONBLOCKING != 0),
            closed: AtomicBool::new(false),
        }
    }

    #[must_use]
    pub const fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    #[must_use]
    pub fn pair(&self) -> &Arc<Pair> {
        &self.pair
    }

    pub fn bind(&self, identity: DescriptionIdentity, bindings: &Arc<Bindings>) {
        bindings.insert(
            identity,
            Arc::new(Handle {
                pair: Arc::clone(&self.pair),
                endpoint: self.endpoint,
                catalog: self.catalog.clone(),
                signals: Arc::clone(&self.signals),
            }),
        );
        *self.binding.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((identity, Arc::downgrade(bindings)));
    }

    fn error(error: ReadError) -> ObjectError {
        match error {
            ReadError::WouldBlock => ObjectError::WouldBlock,
            ReadError::Interrupted => ObjectError::Interrupted,
            ReadError::Retired => ObjectError::Retired,
        }
    }

    fn close_endpoint(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.pair.close_endpoint(self.endpoint);
        if self.endpoint == Endpoint::Master
            && let Some(catalog) = self.catalog.upgrade()
        {
            let _ = catalog.retire(self.pair.id());
        }
    }

    fn write_actor(&self, input: &[u8], actor: Option<OperationActor>) -> Result<usize, ObjectError> {
        match self.endpoint {
            Endpoint::Master => {
                let outcome = self.pair.write_master(input).map_err(Self::error)?;
                for signal in outcome.signals {
                    self.signals
                        .publish(actor, self.pair.id(), self.pair.foreground(), signal);
                }
                Ok(outcome.accepted)
            }
            Endpoint::Slave => self.pair.write_slave(input).map_err(Self::error),
        }
    }
}

impl fmt::Debug for Description {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Description")
            .field("pair", &self.pair.id())
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl OpenFileDescription for Description {
    fn kind(&self) -> ObjectKind {
        ObjectKind::File
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.pair
            .read_blocking(self.endpoint, output, self.nonblocking.load(Ordering::Acquire), None)
            .map_err(Self::error)
    }

    fn probe_read(&self, _maximum: usize) -> Result<Option<usize>, ObjectError> {
        match self.pair.probe_read(self.endpoint) {
            Ok(count) => Ok(Some(count)),
            Err(ReadError::WouldBlock) if !self.nonblocking.load(Ordering::Acquire) => Ok(Some(1)),
            Err(error) => Err(Self::error(error)),
        }
    }

    fn read_with_cancellation(
        &self,
        output: &mut [u8],
        cancellation: &dyn OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.pair
            .read_blocking(
                self.endpoint,
                output,
                self.nonblocking.load(Ordering::Acquire),
                Some(cancellation),
            )
            .map_err(Self::error)
    }

    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        self.write_actor(input, None)
    }

    fn write_context(&self, input: &[u8], context: OperationContext<'_>) -> Result<usize, ObjectError> {
        self.write_actor(input, context.actor)
    }

    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        let (major, minor, permissions) = match self.endpoint {
            Endpoint::Master => (5_u32, 2_u32, 0o666),
            Endpoint::Slave => (136, u32::from(self.pair.id().index), 0o620),
        };
        let device = ((u64::from(major) & 0xfff) << 8) | (u64::from(minor) & 0xff) | ((u64::from(minor) & !0xff) << 12);
        let timestamp = OfdTimestamp {
            seconds: 0,
            nanoseconds: 0,
        };
        Ok(OfdMetadata {
            device: 0,
            inode: (u64::from(major) << 32) | u64::from(minor),
            kind: 2,
            permissions,
            links: 1,
            user: 0,
            group: 0,
            special_device: device,
            size: 0,
            blocks_512: 0,
            block_size: 4096,
            accessed: timestamp,
            modified: timestamp,
            changed: timestamp,
        })
    }

    fn readiness(&self, interests: Readiness) -> Readiness {
        self.pair.readiness(self.endpoint, interests)
    }

    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.pair.subscribe_readiness(self.endpoint, observer)
    }

    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        self.nonblocking
            .store(flags.bits() & StatusFlags::NONBLOCKING != 0, Ordering::Release);
        Ok(())
    }

    fn close(&self) {
        if let Some((identity, bindings)) = self
            .binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            && let Some(bindings) = bindings.upgrade()
        {
            bindings.remove(identity);
        }
        self.close_endpoint();
    }
}

impl Drop for Description {
    fn drop(&mut self) {
        self.close_endpoint();
    }
}

/// Durable control handle independent of a borrowed descriptor operation.
pub struct Handle {
    pub pair: Arc<Pair>,
    pub endpoint: Endpoint,
    pub catalog: Weak<Catalog>,
    pub signals: Arc<dyn SignalSink>,
}

impl Handle {
    pub fn acquire_controlling(&self, session: u32) -> Result<(), CatalogError> {
        self.catalog
            .upgrade()
            .ok_or(CatalogError::NotFound)?
            .acquire(session, self.pair.id())
    }

    /// Acquires this pair and reports whether the catalog created the binding.
    /// Cross-domain callers use the bit to compensate only their own mutation.
    pub fn acquire_controlling_changed(&self, session: u32) -> Result<bool, CatalogError> {
        self.catalog
            .upgrade()
            .ok_or(CatalogError::NotFound)?
            .acquire_changed(session, self.pair.id())
    }

    pub fn detach_controlling(&self, session: u32) -> Result<(), CatalogError> {
        self.catalog
            .upgrade()
            .ok_or(CatalogError::NotFound)?
            .detach(session, self.pair.id())
    }

    #[must_use]
    pub fn controlling_session(&self) -> Option<u32> {
        self.catalog.upgrade()?.controlling_session(self.pair.id())
    }

    pub fn slave(&self) -> Result<Arc<Description>, CatalogError> {
        if self.endpoint != Endpoint::Master {
            return Err(CatalogError::WrongEndpoint);
        }
        let catalog = self.catalog.upgrade().ok_or(CatalogError::NotFound)?;
        catalog.get(self.pair.id())?;
        Ok(Arc::new(Description::new(
            Arc::clone(&self.pair),
            Endpoint::Slave,
            Arc::downgrade(&catalog),
            Arc::clone(&self.signals),
        )))
    }
}

impl fmt::Debug for Handle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Handle")
            .field("pair", &self.pair.id())
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

/// Generation-safe OFD-to-terminal control binding.
#[derive(Default)]
pub struct Bindings {
    entries: Mutex<BTreeMap<DescriptionIdentity, Arc<Handle>>>,
}

impl Bindings {
    pub fn get(&self, identity: DescriptionIdentity) -> Option<Arc<Handle>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&identity)
            .cloned()
    }

    fn insert(&self, identity: DescriptionIdentity, handle: Arc<Handle>) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(identity, handle);
    }

    fn remove(&self, identity: DescriptionIdentity) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&identity);
    }
}
