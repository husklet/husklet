//! Security boundary for authenticated checkpoint generations.
//!
//! This module deliberately contains no checkpoint-store implementation. The
//! byte store is adversarial; authority records live in a distinct trusted,
//! crash-consistent store and recovery authenticates finalized local snapshot
//! handles before native code can read them.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::{fmt, sync::Arc, time::Instant};
use zeroize::{Zeroize, Zeroizing};

pub(super) const GRAMMAR_VERSION: u16 = 1;
pub(super) const HMAC_SHA256: u16 = 1;
const DOMAIN: &[u8; 24] = b"husklet-checkpoint-root\0";
pub(super) const NAME_MAX: usize = 512;
const OBJECT_MAX: usize = 65_536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CommitOutcome {
    Published,
    PublishedNotDurable,
    DefinitelyNotPublished,
    /// Publication may have crossed its irrevocable point. This is an
    /// indeterminate state requiring reconciliation, not committed success.
    PublicationUnknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReplayPolicy {
    Reusable,
    OneShot,
}

pub(super) struct AuthorityKey(Box<[u8; 32]>);

impl AuthorityKey {
    #[cfg(test)]
    pub(super) fn new(bytes: [u8; 32]) -> Self {
        Self(Box::new(bytes))
    }

    pub(super) fn from_box(bytes: Box<[u8; 32]>) -> Self {
        Self(bytes)
    }

    fn expose(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for AuthorityKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthorityKey([REDACTED])")
    }
}

impl Drop for AuthorityKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AuthorityContext {
    pub(super) format: u32,
    pub(super) isa: u32,
    pub(super) generation: u64,
    pub(super) uuid: [u8; 16],
}

pub(super) struct AuthorityMaterial {
    pub(super) context: AuthorityContext,
    pub(super) key: AuthorityKey,
    pub(super) root: [u8; 32],
    pub(super) replay: ReplayPolicy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(super) struct PrepareId(pub(super) [u8; 16]);

pub const AUTHORITY_HANDLE_VERSION: u16 = 1;

/// Non-secret authority reference safe to persist beside application state.
/// It contains no key bytes and cloning shares only the opaque record ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointAuthorityHandle {
    version: u16,
    algorithm: u16,
    context: AuthorityContext,
    root: [u8; 32],
    replay: ReplayPolicy,
    record: Arc<PrepareId>,
}

impl CheckpointAuthorityHandle {
    pub(super) fn new(record: PrepareId, material: &AuthorityMaterial) -> Self {
        Self {
            version: AUTHORITY_HANDLE_VERSION,
            algorithm: HMAC_SHA256,
            context: material.context,
            root: material.root,
            replay: material.replay,
            record: Arc::new(record),
        }
    }
    pub fn version(&self) -> u16 {
        self.version
    }
    pub fn algorithm(&self) -> u16 {
        self.algorithm
    }
    pub fn format(&self) -> u32 {
        self.context.format
    }
    pub fn isa(&self) -> u32 {
        self.context.isa
    }
    pub fn generation(&self) -> u64 {
        self.context.generation
    }
    pub fn uuid(&self) -> [u8; 16] {
        self.context.uuid
    }
    pub fn root(&self) -> [u8; 32] {
        self.root
    }
    pub fn reusable(&self) -> bool {
        self.replay == ReplayPolicy::Reusable
    }
    pub(super) fn record_id(&self) -> PrepareId {
        *self.record
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecordState {
    Prepared,
    Finalized,
    Consumed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AuthorityRecord {
    pub(super) handle: CheckpointAuthorityHandle,
    pub(super) state: RecordState,
}

/// Linear one-shot recovery authority. It is intentionally neither `Copy` nor
/// `Clone`: renew, consume, and abort move the previous fence into one atomic
/// store transition, so stale callers cannot reuse it.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct OneShotFence {
    record: PrepareId,
    fence: u64,
    recovery_uuid: [u8; 16],
    expires_at: Instant,
}

impl OneShotFence {
    /// Store-facing constructor. Callers cannot mint a fence from serialized
    /// checkpoint data because this is visible only to the checkpoint broker.
    pub(super) fn from_trusted_store(
        record: PrepareId,
        fence: u64,
        recovery_uuid: [u8; 16],
        expires_at: Instant,
    ) -> Self {
        Self {
            record,
            fence,
            recovery_uuid,
            expires_at,
        }
    }

    pub(super) fn record(&self) -> PrepareId {
        self.record
    }
    pub(super) fn sequence(&self) -> u64 {
        self.fence
    }
    pub(super) fn recovery_uuid(&self) -> [u8; 16] {
        self.recovery_uuid
    }
    pub(super) fn expires_at(&self) -> Instant {
        self.expires_at
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct GcLease {
    fence: u64,
    expires_at: Instant,
}

impl GcLease {
    pub(super) fn from_trusted_store(fence: u64, expires_at: Instant) -> Self {
        Self { fence, expires_at }
    }

    pub(super) fn sequence(&self) -> u64 {
        self.fence
    }
    pub(super) fn expires_at(&self) -> Instant {
        self.expires_at
    }
}

/// Proof that a different finalized generation authenticated successfully.
/// The fields and constructors stay private so untrusted checkpoint bytes
/// cannot manufacture deletion authority.
pub(super) struct AuthenticatedGeneration {
    record: PrepareId,
    uuid: [u8; 16],
}

impl AuthenticatedGeneration {
    pub(super) fn topology_generation(
        &self,
    ) -> Result<super::topology::CheckpointGeneration, super::topology::AdmissionError> {
        super::topology::CheckpointGeneration::authenticated(self.uuid)
    }

    pub(super) fn topology_lineage(
        &self,
        lineage: [u8; 16],
    ) -> Result<super::topology::LineageId, super::topology::AdmissionError> {
        super::topology::LineageId::authenticated(lineage)
    }
}

pub(super) struct GcEvidence {
    prepared: PrepareId,
    authenticated: PrepareId,
    different_uuid: [u8; 16],
}

impl GcEvidence {
    pub(super) fn for_prepared(
        prepared: PrepareId,
        prepared_uuid: [u8; 16],
        authenticated: &AuthenticatedGeneration,
    ) -> Option<Self> {
        (prepared != authenticated.record && prepared_uuid != authenticated.uuid).then_some(Self {
            prepared,
            authenticated: authenticated.record,
            different_uuid: authenticated.uuid,
        })
    }

    pub(super) fn prepared(&self) -> PrepareId {
        self.prepared
    }
    pub(super) fn authenticated(&self) -> PrepareId {
        self.authenticated
    }
    pub(super) fn different_uuid(&self) -> [u8; 16] {
        self.different_uuid
    }
}

/// Trusted storage contract. Implementations guarantee confidentiality and
/// integrity at rest, linearizable UUID uniqueness, immutable record material,
/// and atomic/idempotent crash-consistent transitions. State is controlled by
/// the store, never supplied by callers. GC mutations require a current
/// monotonically fenced lease and verified evidence of a different generation;
/// elapsed time alone is never deletion authority.
pub(super) trait AuthorityStore: Send + Sync {
    type Error;

    fn prepare(
        &self,
        material: AuthorityMaterial,
        deadline: Instant,
        cancellation: &dyn Cancellation,
    ) -> Result<PrepareId, Self::Error>;
    fn visit_candidates(
        &self,
        context: AuthorityContext,
        maximum: usize,
        visitor: &mut dyn FnMut(AuthorityRecord) -> Result<(), Self::Error>,
        deadline: Instant,
        cancellation: &dyn Cancellation,
    ) -> Result<(), Self::Error>;
    fn record(
        &self,
        id: PrepareId,
        deadline: Instant,
        cancellation: &dyn Cancellation,
    ) -> Result<AuthorityRecord, Self::Error>;
    fn finalize(&self, id: PrepareId, deadline: Instant, cancellation: &dyn Cancellation) -> Result<(), Self::Error>;
    fn discard_definitely_unpublished(
        &self,
        id: PrepareId,
        deadline: Instant,
        cancellation: &dyn Cancellation,
    ) -> Result<(), Self::Error>;
    fn begin_gc(&self, deadline: Instant, cancellation: &dyn Cancellation) -> Result<GcLease, Self::Error>;
    /// Atomically replaces `lease` with a greater fence or fails without
    /// leaving either fence usable.
    fn renew_gc(
        &self,
        lease: GcLease,
        deadline: Instant,
        cancellation: &dyn Cancellation,
    ) -> Result<GcLease, Self::Error>;
    /// Atomically checks the live GC fence, the Prepared state, and the opaque
    /// authenticated evidence before deleting the record and its key.
    fn delete_if_prepared(
        &self,
        lease: GcLease,
        evidence: GcEvidence,
        deadline: Instant,
        cancellation: &dyn Cancellation,
    ) -> Result<(), Self::Error>;
    /// Must succeed only for a Finalized record. Prepared and Consumed records
    /// never release recovery key material.
    fn finalized_recovery_key(
        &self,
        id: PrepareId,
        deadline: Instant,
        cancellation: &dyn Cancellation,
    ) -> Result<RecoveryKey, Self::Error>;
    /// Atomically checks Finalized + OneShot, records `recovery_uuid`, and
    /// returns the only live fence for `(record, recovery_uuid)`.
    fn begin_one_shot(
        &self,
        id: PrepareId,
        recovery_uuid: [u8; 16],
        deadline: Instant,
        cancellation: &dyn Cancellation,
    ) -> Result<OneShotFence, Self::Error>;
    /// Atomically consumes the old fence and returns a greater fence bound to
    /// the same record and recovery UUID.
    fn renew_one_shot(
        &self,
        fence: OneShotFence,
        deadline: Instant,
        cancellation: &dyn Cancellation,
    ) -> Result<OneShotFence, Self::Error>;
    /// Atomically validates record + fence + recovery UUID + settlement proof,
    /// transitions Finalized to Consumed, and destroys the key.
    fn consume_one_shot(
        &self,
        fence: OneShotFence,
        evidence: SettlementEvidence,
        deadline: Instant,
        cancellation: &dyn Cancellation,
    ) -> Result<(), Self::Error>;
    /// Mints opaque store-authenticated settlement evidence only after the
    /// broker supplies its non-deserializable proof that accept scope is
    /// closed, all channels are unbound, and every recovery worker joined.
    fn attest_settlement(
        &self,
        fence: &OneShotFence,
        settlement: SettledRecovery,
        deadline: Instant,
        cancellation: &dyn Cancellation,
    ) -> Result<SettlementEvidence, Self::Error>;
    /// Atomically consumes the live recovery fence without consuming the
    /// checkpoint, permitting a later recovery to acquire a new fence.
    fn abort_one_shot(
        &self,
        fence: OneShotFence,
        deadline: Instant,
        cancellation: &dyn Cancellation,
    ) -> Result<(), Self::Error>;
}

/// Finalized-only recovery key returned by trusted authority storage.
pub(super) struct RecoveryKey(AuthorityKey);

impl RecoveryKey {
    /// Trusted-store boundary: the store must call this only after atomically
    /// reading a Finalized record. The wrapper has no decode implementation.
    pub(super) fn from_finalized_store(bytes: Box<[u8; 32]>) -> Self {
        Self(AuthorityKey::from_box(bytes))
    }

    fn expose(&self) -> &AuthorityKey {
        &self.0
    }
}

/// Constructed only by the Rust broker after RECOVERY_COMPLETE, closed accept
/// scope, zero bound channels, and successful joins. It has no public
/// constructor and is never decoded from C bytes.
pub(super) struct SettlementEvidence {
    record: PrepareId,
    fence: u64,
    recovery_uuid: [u8; 16],
    authenticator: Box<[u8; 32]>,
}

impl SettlementEvidence {
    fn from_trusted_store(fence: &OneShotFence, authenticator: Box<[u8; 32]>) -> Self {
        Self {
            record: fence.record,
            fence: fence.fence,
            recovery_uuid: fence.recovery_uuid,
            authenticator,
        }
    }
    pub(super) fn record(&self) -> PrepareId {
        self.record
    }
    pub(super) fn fence(&self) -> u64 {
        self.fence
    }
    pub(super) fn recovery_uuid(&self) -> [u8; 16] {
        self.recovery_uuid
    }
    fn authenticator(&self) -> &[u8; 32] {
        &self.authenticator
    }

    pub(super) fn matches(&self, fence: &OneShotFence) -> bool {
        self.record == fence.record && self.fence == fence.fence && self.recovery_uuid == fence.recovery_uuid
    }
}

/// Broker-only proof of the recovery shutdown barrier. It has no fields, wire
/// form, public constructor, Clone, or Copy implementation.
pub(super) struct SettledRecovery(());

impl SettledRecovery {
    // Intentionally private. The authority-owned broker barrier will be the
    // sole production caller when protocol wiring lands; sibling checkpoint
    // modules cannot mint settlement before that enforced transition.
    fn after_joined_scope_closed() -> Self {
        Self(())
    }
}

/// One finalized, immutable snapshot object. `read_at` must read the retained
/// O_RDONLY/sealed handle later served to C, not the mutable source stream.
pub(super) trait SnapshotObject: Send + Sync {
    fn name(&self) -> &[u8];
    fn len(&self) -> u64;
    fn read_at(
        &self,
        offset: u64,
        output: &mut [u8],
        deadline: Instant,
        cancellation: &dyn Cancellation,
    ) -> std::io::Result<usize>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NameRead {
    Name(usize),
    End,
}

pub(super) trait Cancellation: Send + Sync {
    fn cancelled(&self) -> bool;
}

/// A single committed generation. One absolute admission deadline is passed
/// unchanged to every operation. `End` is permanent, and object readers use
/// zero as permanent EOF. Each enumerated name may be opened exactly once.
pub(super) trait ObjectReader: Send {
    type Error;
    /// Zero is permanent EOF. Implementations must observe the unchanged
    /// absolute deadline and wake on their generation's cancellation signal.
    fn read_into(
        &mut self,
        output: &mut [u8],
        deadline: Instant,
        cancellation: &dyn Cancellation,
    ) -> Result<usize, Self::Error>;
}

pub(super) trait GenerationReader: Send {
    type Object: ObjectReader<Error = Self::Error>;
    type Error;

    fn next_name(
        &mut self,
        output: &mut [u8; NAME_MAX + 1],
        deadline: Instant,
        cancellation: &dyn Cancellation,
    ) -> Result<NameRead, Self::Error>;
    fn open_object(
        &mut self,
        exact_enumerated_name: &[u8],
        deadline: Instant,
        cancellation: &dyn Cancellation,
    ) -> Result<Self::Object, Self::Error>;
    fn open_manifest(
        &mut self,
        deadline: Instant,
        cancellation: &dyn Cancellation,
    ) -> Result<Self::Object, Self::Error>;
}

pub(super) fn canonical_name(name: &[u8]) -> bool {
    if name.is_empty()
        || name.len() > NAME_MAX
        || name == b"MANIFEST"
        || name.starts_with(b".husklet-recovery/")
        || name == b".husklet-recovery"
        || name.first() == Some(&b'/')
        || name.last() == Some(&b'/')
        || name.contains(&0)
        || std::str::from_utf8(name).is_err()
    {
        return false;
    }
    !name
        .split(|byte| *byte == b'/')
        .any(|component| component.is_empty() || component == b"." || component == b"..")
}

struct HmacSha256 {
    context: Option<Hmac<Sha256>>,
    #[cfg(test)]
    transcript: Transcript,
}

impl HmacSha256 {
    fn new(key: &[u8; 32]) -> Self {
        // `hmac` is the audited RustCrypto streaming construction. Enabling
        // its `zeroize` feature enables `digest/zeroize`; SHA-256's core and
        // digest block buffer then wipe themselves in their respective Drop
        // implementations, including both HMAC keyed prefix states.
        fn require_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}
        require_zeroize_on_drop::<Sha256>();
        Self {
            context: Some(Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts every key length")),
            #[cfg(test)]
            transcript: Transcript::default(),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        Mac::update(self.context.as_mut().expect("unfinished HMAC"), bytes);
        #[cfg(test)]
        self.transcript.update(bytes);
    }

    fn finish(mut self) -> [u8; 32] {
        self.context
            .take()
            .expect("unfinished HMAC")
            .finalize()
            .into_bytes()
            .into()
    }

    fn verify(mut self, expected: &[u8]) -> bool {
        self.context
            .take()
            .expect("unfinished HMAC")
            .verify_slice(expected)
            .is_ok()
    }
}

impl Drop for HmacSha256 {
    fn drop(&mut self) {
        // Dropping the audited context recursively drops the two zeroizing
        // SHA-256 keyed states and its zeroizing digest block buffer.
        drop(self.context.take());
    }
}

impl zeroize::ZeroizeOnDrop for HmacSha256 {}

const _: () = {
    // hmac 0.13 does not forward ZeroizeOnDrop on its public wrapper even
    // with its zeroize feature, so requiring that marker does not compile.
    // Prove the actual context retains Drop glue, and independently require
    // the concrete SHA-256 state it contains to carry the wipe guarantee.
    assert!(std::mem::needs_drop::<Hmac<Sha256>>());
    fn zeroizing_hash_state<T: zeroize::ZeroizeOnDrop>() {}
    fn assert_keyed_state() {
        zeroizing_hash_state::<Sha256>();
    }
};

#[cfg(test)]
struct Transcript {
    bytes: [u8; 512],
    len: usize,
    overflowed: bool,
}

#[cfg(test)]
impl Default for Transcript {
    fn default() -> Self {
        Self {
            bytes: [0; 512],
            len: 0,
            overflowed: false,
        }
    }
}

#[cfg(test)]
impl Transcript {
    fn update(&mut self, input: &[u8]) {
        let Some(end) = self.len.checked_add(input.len()) else {
            self.overflowed = true;
            return;
        };
        if end > self.bytes.len() {
            self.overflowed = true;
            return;
        }
        self.bytes[self.len..end].copy_from_slice(input);
        self.len = end;
    }

    fn bytes(&self) -> &[u8] {
        assert!(!self.overflowed, "test transcript exceeded fixed audit buffer");
        &self.bytes[..self.len]
    }
}

fn check_admission(deadline: Instant, cancellation: &dyn Cancellation) -> std::io::Result<()> {
    if cancellation.cancelled() {
        return Err(std::io::ErrorKind::Interrupted.into());
    }
    if Instant::now() >= deadline {
        return Err(std::io::ErrorKind::TimedOut.into());
    }
    Ok(())
}

fn snapshot_name<'a>(
    object: &'a dyn SnapshotObject,
    deadline: Instant,
    cancellation: &dyn Cancellation,
) -> std::io::Result<&'a [u8]> {
    check_admission(deadline, cancellation)?;
    let name = object.name();
    check_admission(deadline, cancellation)?;
    Ok(name)
}

fn snapshot_length(
    object: &dyn SnapshotObject,
    deadline: Instant,
    cancellation: &dyn Cancellation,
) -> std::io::Result<u64> {
    check_admission(deadline, cancellation)?;
    let length = object.len();
    check_admission(deadline, cancellation)?;
    Ok(length)
}

fn read_exact_snapshot(
    mac: &mut HmacSha256,
    object: &dyn SnapshotObject,
    length: u64,
    deadline: Instant,
    cancellation: &dyn Cancellation,
) -> std::io::Result<()> {
    let mut offset = 0_u64;
    let mut buffer = Zeroizing::new([0_u8; 64 * 1024]);
    while offset < length {
        check_admission(deadline, cancellation)?;
        let remaining =
            usize::try_from((length - offset).min(buffer.len() as u64)).map_err(|_| std::io::ErrorKind::InvalidData)?;
        let read = object.read_at(offset, &mut buffer[..remaining], deadline, cancellation)?;
        check_admission(deadline, cancellation)?;
        if read == 0 || read > remaining {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        mac.update(&buffer[..read]);
        buffer[..read].zeroize();
        offset = offset
            .checked_add(u64::try_from(read).map_err(|_| std::io::ErrorKind::InvalidData)?)
            .ok_or(std::io::ErrorKind::InvalidData)?;
    }

    // Declared length is part of the authenticated metadata. Prove that the
    // immutable retained handle has neither grown nor changed its metadata.
    let mut excess = [0_u8; 1];
    check_admission(deadline, cancellation)?;
    let read = object.read_at(length, &mut excess, deadline, cancellation)?;
    check_admission(deadline, cancellation)?;
    if read != 0 || snapshot_length(object, deadline, cancellation)? != length {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    Ok(())
}

fn update_object(
    mac: &mut HmacSha256,
    object: &dyn SnapshotObject,
    name: &[u8],
    length: u64,
    deadline: Instant,
    cancellation: &dyn Cancellation,
) -> std::io::Result<()> {
    check_admission(deadline, cancellation)?;
    mac.update(&[1]);
    mac.update(
        &u32::try_from(name.len())
            .map_err(|_| std::io::ErrorKind::InvalidData)?
            .to_le_bytes(),
    );
    mac.update(&length.to_le_bytes());
    mac.update(name);
    read_exact_snapshot(mac, object, length, deadline, cancellation)?;
    if snapshot_name(object, deadline, cancellation)? != name {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    Ok(())
}

fn root_mac(
    key: &AuthorityKey,
    context: AuthorityContext,
    objects: &[&dyn SnapshotObject],
    manifest: &dyn SnapshotObject,
    deadline: Instant,
    cancellation: &dyn Cancellation,
) -> std::io::Result<HmacSha256> {
    check_admission(deadline, cancellation)?;
    let manifest_name = snapshot_name(manifest, deadline, cancellation)?;
    let manifest_length = snapshot_length(manifest, deadline, cancellation)?;
    if manifest_name != b"MANIFEST" || objects.len() > OBJECT_MAX {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let mut metadata = Vec::with_capacity(objects.len());
    let mut previous_name = None::<Vec<u8>>;
    for object in objects {
        check_admission(deadline, cancellation)?;
        let borrowed_name = snapshot_name(*object, deadline, cancellation)?;
        if !canonical_name(borrowed_name) {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        let name = borrowed_name.to_vec();
        if previous_name
            .as_deref()
            .is_some_and(|previous| previous >= name.as_slice())
        {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        previous_name = Some(name.clone());
        metadata.push((name, snapshot_length(*object, deadline, cancellation)?));
    }
    let mut mac = HmacSha256::new(key.expose());
    mac.update(DOMAIN);
    mac.update(&GRAMMAR_VERSION.to_le_bytes());
    mac.update(&HMAC_SHA256.to_le_bytes());
    mac.update(&context.format.to_le_bytes());
    mac.update(&context.isa.to_le_bytes());
    mac.update(&context.generation.to_le_bytes());
    mac.update(&context.uuid);
    mac.update(
        &u64::try_from(objects.len())
            .map_err(|_| std::io::ErrorKind::InvalidData)?
            .to_le_bytes(),
    );
    for (object, (name, length)) in objects.iter().zip(&metadata) {
        update_object(&mut mac, *object, name, *length, deadline, cancellation)?;
    }
    check_admission(deadline, cancellation)?;
    mac.update(&[2]);
    mac.update(&manifest_length.to_le_bytes());
    read_exact_snapshot(&mut mac, manifest, manifest_length, deadline, cancellation)?;
    if snapshot_name(manifest, deadline, cancellation)? != b"MANIFEST" {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    check_admission(deadline, cancellation)?;
    mac.update(&[255]);
    Ok(mac)
}

pub(super) fn root(
    key: &AuthorityKey,
    context: AuthorityContext,
    objects: &[&dyn SnapshotObject],
    manifest: &dyn SnapshotObject,
    deadline: Instant,
    cancellation: &dyn Cancellation,
) -> std::io::Result<[u8; 32]> {
    Ok(root_mac(key, context, objects, manifest, deadline, cancellation)?.finish())
}

pub(super) fn authenticate_root(
    record: PrepareId,
    key: &RecoveryKey,
    expected: &[u8],
    context: AuthorityContext,
    objects: &[&dyn SnapshotObject],
    manifest: &dyn SnapshotObject,
    deadline: Instant,
    cancellation: &dyn Cancellation,
) -> std::io::Result<Option<AuthenticatedGeneration>> {
    let verified = root_mac(key.expose(), context, objects, manifest, deadline, cancellation)?.verify(expected);
    Ok(verified.then_some(AuthenticatedGeneration {
        record,
        uuid: context.uuid,
    }))
}

#[cfg(test)]
fn root_and_transcript(
    key: &AuthorityKey,
    context: AuthorityContext,
    objects: &[&dyn SnapshotObject],
    manifest: &dyn SnapshotObject,
    deadline: Instant,
    cancellation: &dyn Cancellation,
) -> std::io::Result<([u8; 32], Vec<u8>)> {
    let mac = root_mac(key, context, objects, manifest, deadline, cancellation)?;
    let transcript = mac.transcript.bytes().to_vec();
    Ok((mac.finish(), transcript))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    struct Never;
    impl Cancellation for Never {
        fn cancelled(&self) -> bool {
            false
        }
    }
    static NEVER: Never = Never;

    struct Stored {
        record: AuthorityRecord,
        key: [u8; 32],
        settlement_key: [u8; 32],
        live: Option<(PrepareId, u64, [u8; 16], Instant)>,
        settlement: Option<[u8; 32]>,
        gc: Option<(u64, Instant)>,
        next_fence: u64,
    }

    impl Drop for Stored {
        fn drop(&mut self) {
            self.key.zeroize();
            self.settlement_key.zeroize();
            self.settlement.zeroize();
        }
    }

    struct MemoryStore(Mutex<Option<Stored>>);

    impl MemoryStore {
        fn new() -> Self {
            Self(Mutex::new(None))
        }

        fn admitted(deadline: Instant, cancellation: &dyn Cancellation) -> Result<(), &'static str> {
            check_admission(deadline, cancellation).map_err(|error| match error.kind() {
                std::io::ErrorKind::Interrupted => "cancelled",
                _ => "deadline",
            })
        }

        fn matching_live(stored: &Stored, fence: &OneShotFence) -> bool {
            stored.live.is_some_and(|(record, sequence, uuid, expires)| {
                record == fence.record
                    && sequence == fence.fence
                    && uuid == fence.recovery_uuid
                    && expires == fence.expires_at
                    && Instant::now() < expires
            })
        }

        fn authentic_settlement(stored: &Stored, fence: &OneShotFence, evidence: &SettlementEvidence) -> bool {
            let mut mac = HmacSha256::new(&stored.settlement_key);
            mac.update(&fence.record().0);
            mac.update(&fence.sequence().to_le_bytes());
            mac.update(&fence.recovery_uuid());
            mac.verify(evidence.authenticator()) && stored.settlement.as_ref() == Some(evidence.authenticator())
        }
    }

    impl AuthorityStore for MemoryStore {
        type Error = &'static str;

        fn prepare(
            &self,
            material: AuthorityMaterial,
            deadline: Instant,
            cancellation: &dyn Cancellation,
        ) -> Result<PrepareId, Self::Error> {
            Self::admitted(deadline, cancellation)?;
            let id = PrepareId(material.context.uuid);
            let record = AuthorityRecord {
                handle: CheckpointAuthorityHandle::new(id, &material),
                state: RecordState::Prepared,
            };
            let key = *material.key.expose();
            *self.0.lock().unwrap() = Some(Stored {
                record,
                key,
                settlement_key: [0x5a; 32],
                live: None,
                settlement: None,
                gc: None,
                next_fence: 1,
            });
            Ok(id)
        }

        fn visit_candidates(
            &self,
            context: AuthorityContext,
            maximum: usize,
            visitor: &mut dyn FnMut(AuthorityRecord) -> Result<(), Self::Error>,
            deadline: Instant,
            cancellation: &dyn Cancellation,
        ) -> Result<(), Self::Error> {
            Self::admitted(deadline, cancellation)?;
            if maximum != 0 {
                if let Some(stored) = self.0.lock().unwrap().as_ref() {
                    if stored.record.handle.context == context {
                        visitor(stored.record.clone())?;
                    }
                }
            }
            Self::admitted(deadline, cancellation)
        }

        fn record(
            &self,
            id: PrepareId,
            deadline: Instant,
            cancellation: &dyn Cancellation,
        ) -> Result<AuthorityRecord, Self::Error> {
            Self::admitted(deadline, cancellation)?;
            let record = self.0.lock().unwrap().as_ref().ok_or("missing")?.record.clone();
            (record.handle.record_id() == id).then_some(record).ok_or("missing")
        }

        fn finalize(
            &self,
            id: PrepareId,
            deadline: Instant,
            cancellation: &dyn Cancellation,
        ) -> Result<(), Self::Error> {
            Self::admitted(deadline, cancellation)?;
            let mut guard = self.0.lock().unwrap();
            let stored = guard.as_mut().ok_or("missing")?;
            if stored.record.handle.record_id() != id || stored.record.state != RecordState::Prepared {
                return Err("state");
            }
            stored.record.state = RecordState::Finalized;
            Ok(())
        }

        fn discard_definitely_unpublished(
            &self,
            id: PrepareId,
            deadline: Instant,
            cancellation: &dyn Cancellation,
        ) -> Result<(), Self::Error> {
            Self::admitted(deadline, cancellation)?;
            let mut guard = self.0.lock().unwrap();
            if guard.as_ref().is_some_and(|stored| {
                stored.record.handle.record_id() == id && stored.record.state == RecordState::Prepared
            }) {
                *guard = None;
                Ok(())
            } else {
                Err("state")
            }
        }

        fn begin_gc(&self, deadline: Instant, cancellation: &dyn Cancellation) -> Result<GcLease, Self::Error> {
            Self::admitted(deadline, cancellation)?;
            let mut guard = self.0.lock().unwrap();
            let stored = guard.as_mut().ok_or("missing")?;
            if stored.gc.is_some_and(|(_, expires)| Instant::now() < expires) {
                return Err("fence");
            }
            let sequence = stored.next_fence;
            stored.next_fence += 1;
            stored.gc = Some((sequence, deadline));
            Ok(GcLease::from_trusted_store(sequence, deadline))
        }

        fn renew_gc(
            &self,
            lease: GcLease,
            deadline: Instant,
            cancellation: &dyn Cancellation,
        ) -> Result<GcLease, Self::Error> {
            Self::admitted(deadline, cancellation)?;
            let mut guard = self.0.lock().unwrap();
            let stored = guard.as_mut().ok_or("missing")?;
            if stored.gc != Some((lease.sequence(), lease.expires_at())) || Instant::now() >= lease.expires_at() {
                return Err("fence");
            }
            let sequence = stored.next_fence;
            stored.next_fence += 1;
            stored.gc = Some((sequence, deadline));
            Ok(GcLease::from_trusted_store(sequence, deadline))
        }

        fn delete_if_prepared(
            &self,
            lease: GcLease,
            evidence: GcEvidence,
            deadline: Instant,
            cancellation: &dyn Cancellation,
        ) -> Result<(), Self::Error> {
            Self::admitted(deadline, cancellation)?;
            let mut guard = self.0.lock().unwrap();
            let stored = guard.as_mut().ok_or("missing")?;
            if stored.gc != Some((lease.sequence(), lease.expires_at()))
                || Instant::now() >= lease.expires_at()
                || evidence.prepared() == evidence.authenticated()
            {
                return Err("fence");
            }
            stored.gc = None;
            let delete = stored.record.handle.record_id() == evidence.prepared()
                && stored.record.state == RecordState::Prepared
                && stored.record.handle.uuid() != evidence.different_uuid();
            if delete {
                *guard = None;
                Ok(())
            } else {
                Err("state")
            }
        }

        fn finalized_recovery_key(
            &self,
            id: PrepareId,
            deadline: Instant,
            cancellation: &dyn Cancellation,
        ) -> Result<RecoveryKey, Self::Error> {
            Self::admitted(deadline, cancellation)?;
            let guard = self.0.lock().unwrap();
            let stored = guard.as_ref().ok_or("missing")?;
            if stored.record.handle.record_id() != id || stored.record.state != RecordState::Finalized {
                return Err("state");
            }
            Ok(RecoveryKey::from_finalized_store(Box::new(stored.key)))
        }

        fn begin_one_shot(
            &self,
            id: PrepareId,
            recovery_uuid: [u8; 16],
            deadline: Instant,
            cancellation: &dyn Cancellation,
        ) -> Result<OneShotFence, Self::Error> {
            Self::admitted(deadline, cancellation)?;
            let mut guard = self.0.lock().unwrap();
            let stored = guard.as_mut().ok_or("missing")?;
            if stored.record.handle.record_id() != id
                || stored.record.state != RecordState::Finalized
                || stored.record.handle.reusable()
                || stored.live.is_some_and(|(_, _, _, expires)| Instant::now() < expires)
            {
                return Err("state");
            }
            let sequence = stored.next_fence;
            stored.next_fence += 1;
            stored.live = Some((id, sequence, recovery_uuid, deadline));
            stored.settlement = None;
            Ok(OneShotFence::from_trusted_store(id, sequence, recovery_uuid, deadline))
        }

        fn renew_one_shot(
            &self,
            fence: OneShotFence,
            deadline: Instant,
            cancellation: &dyn Cancellation,
        ) -> Result<OneShotFence, Self::Error> {
            Self::admitted(deadline, cancellation)?;
            let mut guard = self.0.lock().unwrap();
            let stored = guard.as_mut().ok_or("missing")?;
            if !Self::matching_live(stored, &fence) {
                return Err("fence");
            }
            let sequence = stored.next_fence;
            stored.next_fence += 1;
            stored.live = Some((fence.record, sequence, fence.recovery_uuid, deadline));
            stored.settlement = None;
            Ok(OneShotFence::from_trusted_store(
                fence.record,
                sequence,
                fence.recovery_uuid,
                deadline,
            ))
        }

        fn consume_one_shot(
            &self,
            fence: OneShotFence,
            evidence: SettlementEvidence,
            deadline: Instant,
            cancellation: &dyn Cancellation,
        ) -> Result<(), Self::Error> {
            Self::admitted(deadline, cancellation)?;
            let mut guard = self.0.lock().unwrap();
            let stored = guard.as_mut().ok_or("missing")?;
            if !Self::matching_live(stored, &fence)
                || !evidence.matches(&fence)
                || !Self::authentic_settlement(stored, &fence, &evidence)
            {
                return Err("fence");
            }
            stored.live = None;
            stored.settlement = None;
            stored.record.state = RecordState::Consumed;
            stored.key.zeroize();
            Ok(())
        }

        fn attest_settlement(
            &self,
            fence: &OneShotFence,
            _: SettledRecovery,
            deadline: Instant,
            cancellation: &dyn Cancellation,
        ) -> Result<SettlementEvidence, Self::Error> {
            Self::admitted(deadline, cancellation)?;
            let mut guard = self.0.lock().unwrap();
            let stored = guard.as_mut().ok_or("missing")?;
            if !Self::matching_live(stored, fence) {
                return Err("fence");
            }
            let mut mac = HmacSha256::new(&stored.settlement_key);
            mac.update(&fence.record().0);
            mac.update(&fence.sequence().to_le_bytes());
            mac.update(&fence.recovery_uuid());
            let authenticator = Box::new(mac.finish());
            stored.settlement = Some(*authenticator);
            Ok(SettlementEvidence::from_trusted_store(fence, authenticator))
        }

        fn abort_one_shot(
            &self,
            fence: OneShotFence,
            deadline: Instant,
            cancellation: &dyn Cancellation,
        ) -> Result<(), Self::Error> {
            Self::admitted(deadline, cancellation)?;
            let mut guard = self.0.lock().unwrap();
            let stored = guard.as_mut().ok_or("missing")?;
            if !Self::matching_live(stored, &fence) {
                return Err("fence");
            }
            stored.live = None;
            stored.settlement = None;
            Ok(())
        }
    }

    struct Bytes(&'static [u8], &'static [u8]);
    impl SnapshotObject for Bytes {
        fn name(&self) -> &[u8] {
            self.0
        }
        fn len(&self) -> u64 {
            self.1.len() as u64
        }
        fn read_at(&self, offset: u64, output: &mut [u8], _: Instant, _: &dyn Cancellation) -> std::io::Result<usize> {
            let offset = usize::try_from(offset).unwrap();
            if offset >= self.1.len() {
                return Ok(0);
            }
            let count = output.len().min(self.1.len() - offset);
            output[..count].copy_from_slice(&self.1[offset..offset + count]);
            Ok(count)
        }
    }

    struct Short(&'static [u8], u64);
    impl SnapshotObject for Short {
        fn name(&self) -> &[u8] {
            b"short"
        }
        fn len(&self) -> u64 {
            self.1
        }
        fn read_at(&self, offset: u64, output: &mut [u8], _: Instant, _: &dyn Cancellation) -> std::io::Result<usize> {
            let offset = usize::try_from(offset).unwrap();
            if offset >= self.0.len() {
                return Ok(0);
            }
            let count = output.len().min(self.0.len() - offset);
            output[..count].copy_from_slice(&self.0[offset..offset + count]);
            Ok(count)
        }
    }

    struct MutableLength {
        reads: AtomicUsize,
    }
    impl SnapshotObject for MutableLength {
        fn name(&self) -> &[u8] {
            b"changing"
        }
        fn len(&self) -> u64 {
            if self.reads.fetch_add(1, Ordering::SeqCst) == 0 {
                1
            } else {
                2
            }
        }
        fn read_at(&self, offset: u64, output: &mut [u8], _: Instant, _: &dyn Cancellation) -> std::io::Result<usize> {
            if offset == 0 && !output.is_empty() {
                output[0] = b'x';
                Ok(1)
            } else {
                Ok(0)
            }
        }
    }

    struct MutableName {
        reads: AtomicUsize,
    }
    impl SnapshotObject for MutableName {
        fn name(&self) -> &[u8] {
            if self.reads.fetch_add(1, Ordering::SeqCst) == 0 {
                b"name-a"
            } else {
                b"name-b"
            }
        }
        fn len(&self) -> u64 {
            1
        }
        fn read_at(&self, offset: u64, output: &mut [u8], _: Instant, _: &dyn Cancellation) -> std::io::Result<usize> {
            if offset == 0 && !output.is_empty() {
                output[0] = b'x';
                Ok(1)
            } else {
                Ok(0)
            }
        }
    }

    struct Excess;
    impl SnapshotObject for Excess {
        fn name(&self) -> &[u8] {
            b"excess"
        }
        fn len(&self) -> u64 {
            1
        }
        fn read_at(&self, offset: u64, output: &mut [u8], _: Instant, _: &dyn Cancellation) -> std::io::Result<usize> {
            if offset <= 1 && !output.is_empty() {
                output[0] = b'x';
                Ok(1)
            } else {
                Ok(0)
            }
        }
    }

    struct FlagCancellation(AtomicBool);
    impl Cancellation for FlagCancellation {
        fn cancelled(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct CancelDuringRead<'a>(&'a FlagCancellation);
    impl SnapshotObject for CancelDuringRead<'_> {
        fn name(&self) -> &[u8] {
            b"cancel"
        }
        fn len(&self) -> u64 {
            1
        }
        fn read_at(&self, _: u64, output: &mut [u8], _: Instant, _: &dyn Cancellation) -> std::io::Result<usize> {
            self.0.0.store(true, Ordering::SeqCst);
            output[0] = b'x';
            Ok(1)
        }
    }

    struct Slow;
    impl SnapshotObject for Slow {
        fn name(&self) -> &[u8] {
            b"slow"
        }
        fn len(&self) -> u64 {
            1
        }
        fn read_at(&self, _: u64, output: &mut [u8], _: Instant, _: &dyn Cancellation) -> std::io::Result<usize> {
            std::thread::sleep(std::time::Duration::from_millis(5));
            output[0] = b'x';
            Ok(1)
        }
    }

    #[test]
    fn canonical_names_reject_protocol_and_recovery_aliases() {
        assert!(canonical_name(b"proc.1/pages"));
        for name in [
            b"".as_slice(),
            b"MANIFEST",
            b".husklet-recovery",
            b".husklet-recovery/report",
            b"/proc.1",
            b"proc.1/",
            b"a//b",
            b"a/../b",
            b"a\0b",
        ] {
            assert!(!canonical_name(name), "accepted {name:?}");
        }
    }

    #[test]
    fn grammar_version_one_golden_hmac() {
        // Independent provenance: generated with Python 3 stdlib
        // `hmac.new(key, transcript, hashlib.sha256)`. The committed transcript
        // pins every tag, length, endian choice, ordering byte and terminator.
        const TRANSCRIPT: [u8; 146] = [
            0x68, 0x75, 0x73, 0x6b, 0x6c, 0x65, 0x74, 0x2d, 0x63, 0x68, 0x65, 0x63, 0x6b, 0x70, 0x6f, 0x69, 0x6e, 0x74,
            0x2d, 0x72, 0x6f, 0x6f, 0x74, 0x00, 0x01, 0x00, 0x01, 0x00, 0x07, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
            0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x0c, 0x00, 0x00,
            0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x70, 0x72, 0x6f, 0x63, 0x2e, 0x31, 0x2f, 0x61, 0x72,
            0x65, 0x6e, 0x61, 0x61, 0x72, 0x65, 0x6e, 0x61, 0x01, 0x0c, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x70, 0x72, 0x6f, 0x63, 0x2e, 0x31, 0x2f, 0x70, 0x61, 0x67, 0x65, 0x73, 0x70, 0x61, 0x67,
            0x65, 0x73, 0x02, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6d, 0x61, 0x6e, 0x69, 0x66, 0x65, 0x73,
            0x74, 0xff,
        ];
        assert_eq!(&TRANSCRIPT[..24], DOMAIN);
        let key = AuthorityKey::new([0x0b; 32]);
        let context = AuthorityContext {
            format: 7,
            isa: 2,
            generation: 9,
            uuid: [0x11; 16],
        };
        let first = Bytes(b"proc.1/arena", b"arena");
        let second = Bytes(b"proc.1/pages", b"pages");
        let manifest = Bytes(b"MANIFEST", b"manifest");
        let (actual, emitted) = root_and_transcript(
            &key,
            context,
            &[&first, &second],
            &manifest,
            Instant::now() + std::time::Duration::from_secs(1),
            &NEVER,
        )
        .unwrap();
        assert_eq!(emitted, TRANSCRIPT);
        assert_eq!(
            actual,
            [
                0xcc, 0xb8, 0x36, 0xcf, 0xa0, 0x3e, 0x10, 0x50, 0x2a, 0xe0, 0xea, 0x65, 0x54, 0xe1, 0xd4, 0xcc, 0xb8,
                0xbc, 0xcc, 0xd5, 0x90, 0x38, 0x25, 0x58, 0x34, 0x36, 0xc2, 0x63, 0xa8, 0x17, 0xcb, 0x8c
            ]
        );
        for offset in 0..TRANSCRIPT.len() {
            let mut mutated = TRANSCRIPT;
            mutated[offset] ^= 1;
            let mut mac = HmacSha256::new(&[0x0b; 32]);
            mac.update(&mutated);
            assert_ne!(
                mac.finish(),
                actual,
                "field mutation at transcript byte {offset} was unauthenticated"
            );
        }
        let recovery_key = RecoveryKey(AuthorityKey::new([0x0b; 32]));
        let record = PrepareId([7; 16]);
        assert!(
            authenticate_root(
                record,
                &recovery_key,
                &actual,
                context,
                &[&first, &second],
                &manifest,
                Instant::now() + std::time::Duration::from_secs(1),
                &NEVER
            )
            .unwrap()
            .is_some()
        );
        let mut wrong = actual;
        wrong[0] ^= 1;
        assert!(
            authenticate_root(
                record,
                &recovery_key,
                &wrong,
                context,
                &[&first, &second],
                &manifest,
                Instant::now() + std::time::Duration::from_secs(1),
                &NEVER
            )
            .unwrap()
            .is_none()
        );
        assert!(
            authenticate_root(
                record,
                &recovery_key,
                &wrong[..31],
                context,
                &[&first, &second],
                &manifest,
                Instant::now() + std::time::Duration::from_secs(1),
                &NEVER
            )
            .unwrap()
            .is_none()
        );
        let changed = AuthorityContext {
            generation: 10,
            ..context
        };
        assert!(
            authenticate_root(
                record,
                &recovery_key,
                &actual,
                changed,
                &[&first, &second],
                &manifest,
                Instant::now() + std::time::Duration::from_secs(1),
                &NEVER
            )
            .unwrap()
            .is_none()
        );
        let changed_content = Bytes(b"proc.1/pages", b"pageS");
        assert!(
            authenticate_root(
                record,
                &recovery_key,
                &actual,
                context,
                &[&first, &changed_content],
                &manifest,
                Instant::now() + std::time::Duration::from_secs(1),
                &NEVER
            )
            .unwrap()
            .is_none()
        );
        let changed_name = Bytes(b"proc.1/pagez", b"pages");
        assert!(
            authenticate_root(
                record,
                &recovery_key,
                &actual,
                context,
                &[&first, &changed_name],
                &manifest,
                Instant::now() + std::time::Duration::from_secs(1),
                &NEVER
            )
            .unwrap()
            .is_none()
        );
        let changed_length = Bytes(b"proc.1/pages", b"pages!");
        assert!(
            authenticate_root(
                record,
                &recovery_key,
                &actual,
                context,
                &[&first, &changed_length],
                &manifest,
                Instant::now() + std::time::Duration::from_secs(1),
                &NEVER
            )
            .unwrap()
            .is_none()
        );
        assert!(
            authenticate_root(
                record,
                &recovery_key,
                &actual,
                context,
                &[&first],
                &manifest,
                Instant::now() + std::time::Duration::from_secs(1),
                &NEVER
            )
            .unwrap()
            .is_none()
        );
        let changed_manifest = Bytes(b"MANIFEST", b"manifest!");
        assert!(
            authenticate_root(
                record,
                &recovery_key,
                &actual,
                context,
                &[&first, &second],
                &changed_manifest,
                Instant::now() + std::time::Duration::from_secs(1),
                &NEVER
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn object_order_and_short_snapshot_reads_fail_closed() {
        let key = AuthorityKey::new([1; 32]);
        let context = AuthorityContext {
            format: 7,
            isa: 1,
            generation: 1,
            uuid: [2; 16],
        };
        let a = Bytes(b"b", b"x");
        let b = Bytes(b"a", b"x");
        let manifest = Bytes(b"MANIFEST", b"m");
        assert!(
            root(
                &key,
                context,
                &[&a, &b],
                &manifest,
                Instant::now() + std::time::Duration::from_secs(1),
                &NEVER
            )
            .is_err()
        );
        let short = Short(b"one", 4);
        assert!(
            root(
                &key,
                context,
                &[&short],
                &manifest,
                Instant::now() + std::time::Duration::from_secs(1),
                &NEVER
            )
            .is_err()
        );
        assert!(root(&key, context, &[], &manifest, Instant::now(), &NEVER).is_err());
    }

    #[test]
    fn immutable_length_and_exact_eof_are_enforced() {
        let key = AuthorityKey::new([1; 32]);
        let context = AuthorityContext {
            format: 7,
            isa: 1,
            generation: 1,
            uuid: [2; 16],
        };
        let manifest = Bytes(b"MANIFEST", b"m");
        let changing = MutableLength {
            reads: AtomicUsize::new(0),
        };
        let changing_name = MutableName {
            reads: AtomicUsize::new(0),
        };
        let excess = Excess;
        let deadline = || Instant::now() + std::time::Duration::from_secs(1);
        assert!(root(&key, context, &[&changing], &manifest, deadline(), &NEVER).is_err());
        assert!(root(&key, context, &[&changing_name], &manifest, deadline(), &NEVER).is_err());
        assert!(root(&key, context, &[&excess], &manifest, deadline(), &NEVER).is_err());
    }

    #[test]
    fn cancellation_and_deadline_are_checked_after_each_read() {
        let key = AuthorityKey::new([1; 32]);
        let context = AuthorityContext {
            format: 7,
            isa: 1,
            generation: 1,
            uuid: [2; 16],
        };
        let manifest = Bytes(b"MANIFEST", b"m");
        let cancellation = FlagCancellation(AtomicBool::new(false));
        let cancelling = CancelDuringRead(&cancellation);
        let cancelled = root(
            &key,
            context,
            &[&cancelling],
            &manifest,
            Instant::now() + std::time::Duration::from_secs(1),
            &cancellation,
        )
        .unwrap_err();
        assert_eq!(cancelled.kind(), std::io::ErrorKind::Interrupted);

        let slow = Slow;
        let timed_out = root(
            &key,
            context,
            &[&slow],
            &manifest,
            Instant::now() + std::time::Duration::from_millis(1),
            &NEVER,
        )
        .unwrap_err();
        assert_eq!(timed_out.kind(), std::io::ErrorKind::TimedOut);
    }

    #[test]
    fn authenticated_gc_evidence_is_bound_to_distinct_records_and_uuids() {
        let authenticated = AuthenticatedGeneration {
            record: PrepareId([2; 16]),
            uuid: [3; 16],
        };
        assert!(GcEvidence::for_prepared(PrepareId([1; 16]), [4; 16], &authenticated).is_some());
        assert!(GcEvidence::for_prepared(PrepareId([2; 16]), [4; 16], &authenticated).is_none());
        assert!(GcEvidence::for_prepared(PrepareId([1; 16]), [3; 16], &authenticated).is_none());
    }

    #[test]
    fn gc_lease_is_exclusive_and_stale_fences_cannot_delete() {
        let store = MemoryStore::new();
        let deadline = || Instant::now() + std::time::Duration::from_secs(1);
        let id = store
            .prepare(authority_material(ReplayPolicy::Reusable), deadline(), &NEVER)
            .unwrap();
        let authenticated = AuthenticatedGeneration {
            record: PrepareId([2; 16]),
            uuid: [3; 16],
        };
        let evidence = GcEvidence::for_prepared(id, [7; 16], &authenticated).unwrap();
        let first = store.begin_gc(deadline(), &NEVER).unwrap();
        assert!(store.begin_gc(deadline(), &NEVER).is_err());
        let stale = GcLease::from_trusted_store(first.sequence(), first.expires_at());
        let current = store.renew_gc(first, deadline(), &NEVER).unwrap();
        assert!(store.delete_if_prepared(stale, evidence, deadline(), &NEVER).is_err());
        let evidence = GcEvidence::for_prepared(id, [7; 16], &authenticated).unwrap();
        store.delete_if_prepared(current, evidence, deadline(), &NEVER).unwrap();
        assert!(store.record(id, deadline(), &NEVER).is_err());
    }

    fn authority_material(replay: ReplayPolicy) -> AuthorityMaterial {
        AuthorityMaterial {
            context: AuthorityContext {
                format: 7,
                isa: 1,
                generation: 1,
                uuid: [7; 16],
            },
            key: AuthorityKey::new([9; 32]),
            root: [8; 32],
            replay,
        }
    }

    #[test]
    fn store_releases_keys_only_after_finalize_and_honors_admission() {
        let store = MemoryStore::new();
        let cancelled = FlagCancellation(AtomicBool::new(true));
        assert_eq!(
            store.prepare(
                authority_material(ReplayPolicy::OneShot),
                Instant::now() + std::time::Duration::from_secs(1),
                &cancelled,
            ),
            Err("cancelled")
        );
        assert_eq!(
            store.prepare(authority_material(ReplayPolicy::OneShot), Instant::now(), &NEVER),
            Err("deadline")
        );

        let deadline = || Instant::now() + std::time::Duration::from_secs(1);
        let id = store
            .prepare(authority_material(ReplayPolicy::OneShot), deadline(), &NEVER)
            .unwrap();
        assert!(store.finalized_recovery_key(id, deadline(), &NEVER).is_err());
        store.finalize(id, deadline(), &NEVER).unwrap();
        let key = store.finalized_recovery_key(id, deadline(), &NEVER).unwrap();
        assert_eq!(key.expose().expose(), &[9; 32]);
    }

    #[test]
    fn one_shot_store_rejects_stale_fences_and_requires_explicit_abort_or_expiry() {
        let store = MemoryStore::new();
        let deadline = || Instant::now() + std::time::Duration::from_secs(1);
        let id = store
            .prepare(authority_material(ReplayPolicy::OneShot), deadline(), &NEVER)
            .unwrap();
        store.finalize(id, deadline(), &NEVER).unwrap();
        let first = store.begin_one_shot(id, [1; 16], deadline(), &NEVER).unwrap();
        let stale = OneShotFence::from_trusted_store(
            first.record(),
            first.sequence(),
            first.recovery_uuid(),
            first.expires_at(),
        );
        let current = store.renew_one_shot(first, deadline(), &NEVER).unwrap();
        assert!(store.abort_one_shot(stale, deadline(), &NEVER).is_err());
        drop(current);
        assert!(store.begin_one_shot(id, [2; 16], deadline(), &NEVER).is_err());
        {
            let mut guard = store.0.lock().unwrap();
            let stored = guard.as_mut().unwrap();
            let (record, sequence, uuid, _) = stored.live.unwrap();
            stored.live = Some((
                record,
                sequence,
                uuid,
                Instant::now() - std::time::Duration::from_millis(1),
            ));
        }
        let replacement = store.begin_one_shot(id, [2; 16], deadline(), &NEVER).unwrap();
        store.abort_one_shot(replacement, deadline(), &NEVER).unwrap();
    }

    #[test]
    fn one_shot_consume_is_bound_to_fence_record_and_recovery_uuid() {
        let store = MemoryStore::new();
        let deadline = || Instant::now() + std::time::Duration::from_secs(1);
        let id = store
            .prepare(authority_material(ReplayPolicy::OneShot), deadline(), &NEVER)
            .unwrap();
        store.finalize(id, deadline(), &NEVER).unwrap();
        let fence = store.begin_one_shot(id, [1; 16], deadline(), &NEVER).unwrap();
        let wrong = OneShotFence::from_trusted_store(id, fence.sequence(), [2; 16], fence.expires_at());
        let evidence = store
            .attest_settlement(&fence, SettledRecovery::after_joined_scope_closed(), deadline(), &NEVER)
            .unwrap();
        assert!(store.consume_one_shot(wrong, evidence, deadline(), &NEVER).is_err());
        let mut evidence = store
            .attest_settlement(&fence, SettledRecovery::after_joined_scope_closed(), deadline(), &NEVER)
            .unwrap();
        evidence.authenticator[0] ^= 1;
        let duplicate = OneShotFence::from_trusted_store(
            fence.record(),
            fence.sequence(),
            fence.recovery_uuid(),
            fence.expires_at(),
        );
        assert!(store.consume_one_shot(duplicate, evidence, deadline(), &NEVER).is_err());
        let evidence = store
            .attest_settlement(&fence, SettledRecovery::after_joined_scope_closed(), deadline(), &NEVER)
            .unwrap();
        store.consume_one_shot(fence, evidence, deadline(), &NEVER).unwrap();
        assert_eq!(
            store.record(id, deadline(), &NEVER).unwrap().state,
            RecordState::Consumed
        );
        assert!(store.finalized_recovery_key(id, deadline(), &NEVER).is_err());
    }

    #[test]
    fn settlement_evidence_is_bound_to_the_live_fence() {
        let fence = OneShotFence {
            record: PrepareId([1; 16]),
            fence: 9,
            recovery_uuid: [2; 16],
            expires_at: Instant::now() + std::time::Duration::from_secs(1),
        };
        let evidence = SettlementEvidence::from_trusted_store(&fence, Box::new([3; 32]));
        assert_eq!(evidence.record(), fence.record);
        assert_eq!(evidence.fence(), fence.fence);
        assert_eq!(evidence.recovery_uuid(), fence.recovery_uuid);
        assert_eq!(evidence.authenticator(), &[3; 32]);
        assert!(evidence.matches(&fence));
        let stale = OneShotFence {
            record: fence.record,
            fence: fence.fence + 1,
            recovery_uuid: fence.recovery_uuid,
            expires_at: fence.expires_at,
        };
        assert!(!evidence.matches(&stale));
        drop(stale);
        drop(fence);
        // Dropping a client fence performs no implicit abort. The trusted
        // store keeps it fenced until an explicit atomic abort or expiry.
    }

    #[test]
    fn settlement_barrier_token_constructor_stays_authority_private() {
        let source = include_str!("authority.rs");
        assert!(source.contains("fn after_joined_scope_closed() -> Self"));
        assert!(!source.contains(concat!("pub(super) ", "fn after_joined_scope_closed() -> Self")));
        assert!(!source.contains(concat!("pub(crate) ", "fn after_joined_scope_closed() -> Self")));
        assert!(!source.contains(concat!("pub ", "fn after_joined_scope_closed() -> Self")));
    }

    #[test]
    fn empty_and_one_object_boundary_vectors() {
        let key = AuthorityKey::new([1; 32]);
        let context = AuthorityContext {
            format: 7,
            isa: 1,
            generation: 1,
            uuid: [2; 16],
        };
        let manifest = Bytes(b"MANIFEST", b"m");
        let one = Bytes(b"a", b"x");
        let deadline = || Instant::now() + std::time::Duration::from_secs(1);
        assert_eq!(
            root(&key, context, &[], &manifest, deadline(), &NEVER).unwrap(),
            [
                0x39, 0x47, 0x3b, 0x5f, 0x1f, 0xc9, 0x3f, 0x64, 0x96, 0x61, 0x72, 0xaa, 0x59, 0xca, 0xcc, 0x1b, 0xf0,
                0x14, 0x85, 0xaf, 0x8a, 0x50, 0xcc, 0x2e, 0x3d, 0x18, 0xef, 0x96, 0x02, 0x44, 0x13, 0xe1
            ]
        );
        assert_eq!(
            root(&key, context, &[&one], &manifest, deadline(), &NEVER).unwrap(),
            [
                0xf7, 0x53, 0x3b, 0x9f, 0x40, 0x3f, 0x4d, 0xfd, 0x9f, 0x19, 0x7d, 0x02, 0xb2, 0x79, 0x28, 0x9c, 0x0e,
                0xba, 0xa2, 0xa3, 0x99, 0x14, 0x00, 0xa3, 0xc2, 0xce, 0xce, 0xaf, 0x1d, 0x0c, 0xd6, 0xdb
            ]
        );
    }
}
