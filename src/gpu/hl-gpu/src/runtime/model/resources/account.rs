use super::*;
// ---------------------------------------------------------------------------------------------------
// residency accounting state (transaction workflow lives in service/account.rs)
// ---------------------------------------------------------------------------------------------------

/// Residency counters for one connection. `bytes`/`objects` are the aggregate charge; `compiled_bytes`
/// is the subset attributable to the compiled-pipeline (PSO/AIR) cache, bounded by its own ceiling.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Totals {
    pub bytes: u64,
    pub objects: u64,
    pub compiled_bytes: u64,
}

/// The per-connection accounting ledger: the live `(kind, id) → bytes` charges and their running
/// [`Totals`]. Pure state — `service/account.rs` computes a proposed next ledger and commits it.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Ledger {
    pub live: HashMap<(u8, u32), u64>,
    pub totals: Totals,
}

/// Cloneable authority for one connection's residency and cross-session import reservations. Ordinary
/// residency and reservations share one lock so an import cannot race a frame allocation past the
/// connection ceiling. Reservations deliberately do not touch [`GlobalLedger`]: the owner already carries
/// the one physical global charge.
#[derive(Clone)]
pub struct Account {
    pub(crate) inner: Arc<Mutex<AccountState>>,
    operation: Arc<Mutex<()>>,
    session: Arc<AtomicU64>,
}

#[derive(Clone, Default)]
pub(crate) struct AccountState {
    pub(crate) ledger: Ledger,
    pub(crate) reservations: HashMap<u64, u64>,
}

impl Account {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(AccountState::default())),
            operation: Arc::new(Mutex::new(())),
            session: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(crate) fn same_authority(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub(crate) fn bind_session(
        &self,
        session: crate::runtime::model::sharing::SessionId,
    ) -> Result<()> {
        match self
            .session
            .compare_exchange(0, session.0, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Ok(()),
            Err(existing) if existing == session.0 => Ok(()),
            Err(_) => Err(GpuError::Invalid(
                "account authority reused across sharing sessions",
            )),
        }
    }

    pub(crate) fn operation(&self) -> std::sync::MutexGuard<'_, ()> {
        self.operation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn ledger(&self) -> Ledger {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .ledger
            .clone()
    }

    pub(crate) fn replace_ledger(&self, ledger: Ledger) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).ledger = ledger;
    }

    /// Restore a ledger snapshot after a refused operation. The account operation lock makes `current`
    /// the contribution installed by that operation, so replacing it with the previously committed
    /// snapshot cannot exceed either ceiling and needs no fallible cleanup path.
    pub(crate) fn restore_ledger(&self, previous: Ledger, global: &GlobalLedger) {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        global.restore(state.ledger.totals, previous.totals);
        state.ledger = previous;
    }

    pub(crate) fn commit_ledger(
        &self,
        old: Totals,
        ledger: Ledger,
        max_bytes: u64,
        max_objects: u64,
        global: &GlobalLedger,
    ) -> Result<()> {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let reserved_bytes = state
            .reservations
            .values()
            .try_fold(0u64, |sum, value| sum.checked_add(*value))
            .ok_or(GpuError::ResourceLimit("reservation overflow"))?;
        let bytes = ledger
            .totals
            .bytes
            .checked_add(reserved_bytes)
            .ok_or(GpuError::ResourceLimit("reservation overflow"))?;
        let objects = ledger
            .totals
            .objects
            .checked_add(state.reservations.len() as u64)
            .ok_or(GpuError::ResourceLimit("reservation object overflow"))?;
        if bytes > max_bytes || objects > max_objects {
            return Err(GpuError::ResourceLimit("connection residency"));
        }
        global.commit(old, ledger.totals)?;
        state.ledger = ledger;
        Ok(())
    }

    pub(crate) fn reserve(
        &self,
        export: u64,
        bytes: u64,
        max_bytes: u64,
        max_objects: u64,
    ) -> Result<()> {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if state.reservations.contains_key(&export) {
            return Err(GpuError::Invalid("duplicate shared-buffer reservation"));
        }
        let reserved_bytes = state
            .reservations
            .values()
            .try_fold(0u64, |sum, value| sum.checked_add(*value))
            .ok_or(GpuError::ResourceLimit("reservation overflow"))?;
        let bytes_after = state
            .ledger
            .totals
            .bytes
            .checked_add(reserved_bytes)
            .and_then(|sum| sum.checked_add(bytes))
            .ok_or(GpuError::ResourceLimit("reservation overflow"))?;
        let objects_after = state
            .ledger
            .totals
            .objects
            .checked_add(state.reservations.len() as u64)
            .and_then(|sum| sum.checked_add(1))
            .ok_or(GpuError::ResourceLimit("reservation object overflow"))?;
        if bytes_after > max_bytes || objects_after > max_objects {
            return Err(GpuError::ResourceLimit("connection residency"));
        }
        state.reservations.insert(export, bytes);
        Ok(())
    }

    #[cfg(all(debug_assertions, not(doc)))]
    pub(crate) fn release_reservation(&self, export: u64) -> Result<u64> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reservations
            .remove(&export)
            .ok_or(GpuError::Invalid("missing shared-buffer reservation"))
    }

    #[cfg(all(debug_assertions, not(doc)))]
    pub fn debug_commit_ledger(
        &self,
        old: Totals,
        next: Ledger,
        max_bytes: u64,
        max_objects: u64,
        global: &GlobalLedger,
    ) -> Result<()> {
        self.commit_ledger(old, next, max_bytes, max_objects, global)
    }

    #[cfg(all(debug_assertions, not(doc)))]
    pub fn debug_reserve(
        &self,
        export: u64,
        bytes: u64,
        max_bytes: u64,
        max_objects: u64,
    ) -> Result<()> {
        self.reserve(export, bytes, max_bytes, max_objects)
    }

    #[cfg(all(debug_assertions, not(doc)))]
    pub fn debug_release_reservation(&self, export: u64) -> Result<u64> {
        self.release_reservation(export)
    }

    pub(crate) fn discard_reservation(&self, export: u64) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reservations
            .remove(&export);
    }

    pub fn reserved_bytes(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reservations
            .values()
            .copied()
            .sum()
    }
}

impl Default for Account {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod account_transfer_races {
    use super::*;
    use std::sync::mpsc;
    use std::thread;

    fn raced_restore(panic_path: bool) {
        let global = GlobalLedger::new(64, 8);
        let account = Account::new();
        let mut initial = Ledger::default();
        initial.live.insert((KIND_BUFFER, 1), 4);
        initial.totals = Totals {
            bytes: 4,
            objects: 1,
            compiled_bytes: 0,
        };
        account
            .commit_ledger(Totals::default(), initial.clone(), 64, 8, &global)
            .unwrap();
        let (installed_tx, installed_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let submit_account = account.clone();
        let submit_global = global.clone();
        let submit = thread::spawn(move || {
            let _operation = submit_account.operation();
            let previous = submit_account.ledger();
            let mut charged = previous.clone();
            charged.live.insert((KIND_BUFFER, 2), 4);
            charged.totals.bytes = 8;
            charged.totals.objects = 2;
            submit_account
                .commit_ledger(previous.totals, charged, 64, 8, &submit_global)
                .unwrap();
            installed_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            if panic_path {
                let _ = std::panic::catch_unwind(|| panic!("executor panic"));
            }
            submit_account.restore_ledger(previous, &submit_global);
        });
        installed_rx.recv().unwrap();
        let transfer_account = account.clone();
        let transfer = thread::spawn(move || {
            let _operation = transfer_account.operation();
            transfer_account.ledger()
        });
        release_tx.send(()).unwrap();
        submit.join().unwrap();
        let observed = transfer.join().unwrap();
        assert_eq!(observed.totals, initial.totals);
        assert_eq!(observed.live, initial.live);
        assert_eq!(global.snapshot().bytes, 4);
    }

    #[test]
    fn payer_transfer_waits_for_submit_nack_rollback() {
        raced_restore(false);
    }

    #[test]
    fn payer_transfer_waits_for_submit_panic_rollback() {
        raced_restore(true);
    }
}

impl Ledger {
    /// Cumulative bytes resident on this connection.
    pub fn residency_bytes(&self) -> u64 {
        self.totals.bytes
    }
    /// Cumulative live object count charged to this connection.
    pub fn object_count(&self) -> u64 {
        self.totals.objects
    }
    /// Bytes of this connection's residency attributable to the compiled-pipeline cache.
    pub fn compiled_cache_bytes(&self) -> u64 {
        self.totals.compiled_bytes
    }
}

/// The process-global residency ceiling shared across every connection (clone shares the same account).
/// Enforces a fair global budget so one connection cannot starve the host of all GPU memory.
#[derive(Clone)]
pub struct GlobalLedger {
    inner: Arc<Mutex<Totals>>,
    max_bytes: u64,
    max_objects: u64,
}

impl GlobalLedger {
    pub(crate) fn lock_totals(&self) -> std::sync::MutexGuard<'_, Totals> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }
    pub(crate) fn same_authority(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    pub fn new(max_bytes: u64, max_objects: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Totals::default())),
            max_bytes,
            max_objects,
        }
    }

    /// An effectively-unbounded global account (per-connection ceilings still apply).
    pub fn unbounded() -> Self {
        Self::new(u64::MAX, u64::MAX)
    }

    /// Atomically swap this connection's global contribution from `old` to `next`, rejecting if the
    /// resulting process-wide total would exceed a ceiling. On rejection the global account is unchanged.
    /// `compiled_bytes` is a per-connection concern and is not tracked process-globally.
    pub fn commit(&self, old: Totals, next: Totals) -> Result<()> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let without = Totals {
            bytes: g.bytes.saturating_sub(old.bytes),
            objects: g.objects.saturating_sub(old.objects),
            compiled_bytes: 0,
        };
        let proposed = Totals {
            bytes: without
                .bytes
                .checked_add(next.bytes)
                .ok_or(GpuError::ResourceLimit("global residency overflow"))?,
            objects: without
                .objects
                .checked_add(next.objects)
                .ok_or(GpuError::ResourceLimit("global object overflow"))?,
            compiled_bytes: 0,
        };
        if proposed.bytes > self.max_bytes || proposed.objects > self.max_objects {
            return Err(GpuError::ResourceLimit("global residency"));
        }
        *g = proposed;
        Ok(())
    }

    fn restore(&self, current: Totals, previous: Totals) {
        let mut global = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        global.bytes = global
            .bytes
            .saturating_sub(current.bytes)
            .saturating_add(previous.bytes);
        global.objects = global
            .objects
            .saturating_sub(current.objects)
            .saturating_add(previous.objects);
    }

    /// Release this connection's whole contribution back to the global account (on disconnect).
    pub fn refund(&self, totals: Totals) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.bytes = g.bytes.saturating_sub(totals.bytes);
        g.objects = g.objects.saturating_sub(totals.objects);
    }

    /// A snapshot of the process-wide residency currently charged across every connection sharing this
    /// account. A leak check: it must return to its baseline once all sharing connections tear down.
    pub fn snapshot(&self) -> Totals {
        *self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn residency_bytes(&self) -> u64 {
        self.snapshot().bytes
    }

    pub fn object_count(&self) -> u64 {
        self.snapshot().objects
    }
}
