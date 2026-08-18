//! Security boundary for authenticated checkpoint generations.
//!
//! This module deliberately contains no checkpoint-store implementation. The
//! byte store is adversarial; authority records live in a distinct trusted,
//! crash-consistent store and recovery authenticates finalized local snapshot
//! handles before native code can read them.

use ring::{constant_time, hmac};
use std::{fmt, sync::Arc, time::Instant};
use zeroize::Zeroize;

pub(super) const GRAMMAR_VERSION: u16 = 1;
pub(super) const HMAC_SHA256: u16 = 1;
const DOMAIN: &[u8; 24] = b"husklet-checkpoint-root\0";
pub(super) const NAME_MAX: usize = 512;

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
        Self { version: AUTHORITY_HANDLE_VERSION, algorithm: HMAC_SHA256, context: material.context,
            root: material.root, replay: material.replay, record: Arc::new(record) }
    }
    pub fn version(&self) -> u16 { self.version }
    pub fn algorithm(&self) -> u16 { self.algorithm }
    pub fn format(&self) -> u32 { self.context.format }
    pub fn isa(&self) -> u32 { self.context.isa }
    pub fn generation(&self) -> u64 { self.context.generation }
    pub fn uuid(&self) -> [u8; 16] { self.context.uuid }
    pub fn root(&self) -> [u8; 32] { self.root }
    pub fn reusable(&self) -> bool { self.replay == ReplayPolicy::Reusable }
    pub(super) fn record_id(&self) -> PrepareId { *self.record }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecordState { Prepared, Finalized, Consumed }

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AuthorityRecord {
    pub(super) handle: CheckpointAuthorityHandle,
    pub(super) state: RecordState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Lease {
    pub(super) record: PrepareId,
    pub(super) fence: u64,
    pub(super) expires_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct GcLease {
    pub(super) fence: u64,
    pub(super) expires_at: Instant,
}

/// Trusted storage contract. Implementations guarantee confidentiality and
/// integrity at rest, linearizable UUID uniqueness, immutable record material,
/// and atomic/idempotent crash-consistent transitions. State is controlled by
/// the store, never supplied by callers. GC mutations require a current
/// monotonically fenced lease and verified evidence of a different generation;
/// elapsed time alone is never deletion authority.
pub(super) trait AuthorityStore: Send + Sync {
    type Error;

    fn prepare(&self, material: AuthorityMaterial, deadline: Instant) -> Result<PrepareId, Self::Error>;
    fn visit_candidates(
        &self,
        context: AuthorityContext,
        maximum: usize,
        visitor: &mut dyn FnMut(AuthorityRecord) -> Result<(), Self::Error>,
        deadline: Instant,
    ) -> Result<(), Self::Error>;
    fn record(&self, id: PrepareId, deadline: Instant) -> Result<AuthorityRecord, Self::Error>;
    fn finalize(&self, id: PrepareId, deadline: Instant) -> Result<(), Self::Error>;
    fn discard_definitely_unpublished(&self, id: PrepareId, deadline: Instant) -> Result<(), Self::Error>;
    fn begin_gc(&self, deadline: Instant) -> Result<GcLease, Self::Error>;
    fn renew_gc(&self, lease: GcLease, deadline: Instant) -> Result<GcLease, Self::Error>;
    fn delete_if_prepared(&self, id: PrepareId, lease: GcLease, different_verified_uuid: [u8; 16], deadline: Instant)
        -> Result<(), Self::Error>;
    /// Must succeed only for authenticated Prepared or Finalized records.
    fn key_guard(&self, id: PrepareId, deadline: Instant) -> Result<AuthorityKey, Self::Error>;
    fn begin_one_shot(&self, id: PrepareId, deadline: Instant) -> Result<Lease, Self::Error>;
    fn renew_one_shot(&self, lease: Lease, deadline: Instant) -> Result<Lease, Self::Error>;
    fn consume_one_shot(&self, lease: Lease, evidence: SettlementEvidence, deadline: Instant)
    -> Result<(), Self::Error>;
    fn abort_one_shot(&self, lease: Lease, deadline: Instant) -> Result<(), Self::Error>;
}

/// Constructed only by the Rust broker after RECOVERY_COMPLETE, closed accept
/// scope, zero bound channels, and successful joins. It has no public
/// constructor and is never decoded from C bytes.
pub(super) struct SettlementEvidence {
    recovery_uuid: [u8; 16],
    capability: [u8; 16],
}

impl SettlementEvidence {
    pub(super) fn after_full_settlement(recovery_uuid: [u8; 16], capability: [u8; 16]) -> Self {
        Self { recovery_uuid, capability }
    }
    pub(super) fn recovery_uuid(&self) -> [u8; 16] { self.recovery_uuid }
    pub(super) fn capability(&self) -> [u8; 16] { self.capability }
}

/// One finalized, immutable snapshot object. `read_at` must read the retained
/// O_RDONLY/sealed handle later served to C, not the mutable source stream.
pub(super) trait SnapshotObject: Send + Sync {
    fn name(&self) -> &[u8];
    fn len(&self) -> u64;
    fn read_at(&self, offset: u64, output: &mut [u8], deadline: Instant) -> std::io::Result<usize>;
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

    fn next_name(&mut self, output: &mut [u8; NAME_MAX + 1], deadline: Instant,
                 cancellation: &dyn Cancellation) -> Result<NameRead, Self::Error>;
    fn open_object(&mut self, exact_enumerated_name: &[u8], deadline: Instant,
                   cancellation: &dyn Cancellation) -> Result<Self::Object, Self::Error>;
    fn open_manifest(&mut self, deadline: Instant,
                     cancellation: &dyn Cancellation) -> Result<Self::Object, Self::Error>;
}

pub(super) fn canonical_name(name: &[u8]) -> bool {
    if name.is_empty() || name.len() > NAME_MAX || name == b"MANIFEST" || name.starts_with(b".husklet-recovery/")
        || name == b".husklet-recovery"
        || name.first() == Some(&b'/') || name.last() == Some(&b'/') || name.contains(&0)
        || std::str::from_utf8(name).is_err()
    {
        return false;
    }
    !name.split(|byte| *byte == b'/').any(|component| component.is_empty() || component == b"." || component == b"..")
}

struct HmacSha256 {
    context: Option<hmac::Context>,
    #[cfg(test)]
    transcript: Vec<u8>,
}

impl HmacSha256 {
    fn new(key: &[u8]) -> Self {
        let key = hmac::Key::new(hmac::HMAC_SHA256, key);
        Self {
            context: Some(hmac::Context::with_key(&key)),
            #[cfg(test)]
            transcript: Vec::new(),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        self.context.as_mut().expect("unfinished HMAC").update(bytes);
        #[cfg(test)]
        self.transcript.extend_from_slice(bytes);
    }

    fn finish(mut self) -> [u8; 32] {
        self.context.take().expect("unfinished HMAC").sign().as_ref().try_into().expect("SHA-256 tag length")
    }
}

impl Drop for HmacSha256 {
    fn drop(&mut self) {
        if let Some(mac) = self.context.take() {
            let tag = mac.sign();
            let mut derived: [u8; 32] = tag.as_ref().try_into().expect("SHA-256 tag length");
            derived.zeroize();
        }
    }
}

fn update_object(mac: &mut HmacSha256, object: &dyn SnapshotObject, deadline: Instant) -> std::io::Result<()> {
    if Instant::now() >= deadline { return Err(std::io::ErrorKind::TimedOut.into()); }
    mac.update(&[1]);
    mac.update(&(object.name().len() as u32).to_le_bytes());
    mac.update(&object.len().to_le_bytes());
    mac.update(object.name());
    let mut offset = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while offset < object.len() {
        if Instant::now() >= deadline { return Err(std::io::ErrorKind::TimedOut.into()); }
        let remaining = usize::try_from((object.len() - offset).min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = object.read_at(offset, &mut buffer[..remaining], deadline)?;
        if read == 0 || read > remaining { return Err(std::io::ErrorKind::UnexpectedEof.into()); }
        mac.update(&buffer[..read]);
        offset = offset.checked_add(read as u64).ok_or(std::io::ErrorKind::InvalidData)?;
    }
    Ok(())
}

fn root_mac(
    key: &AuthorityKey,
    context: AuthorityContext,
    objects: &[&dyn SnapshotObject],
    manifest: &dyn SnapshotObject,
    deadline: Instant,
) -> std::io::Result<HmacSha256> {
    if Instant::now() >= deadline { return Err(std::io::ErrorKind::TimedOut.into()); }
    if manifest.name() != b"MANIFEST" || objects.len() > u64::MAX as usize { return Err(std::io::ErrorKind::InvalidInput.into()); }
    for (index, object) in objects.iter().enumerate() {
        if Instant::now() >= deadline { return Err(std::io::ErrorKind::TimedOut.into()); }
        if !canonical_name(object.name()) || index != 0 && objects[index - 1].name() >= object.name() {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
    }
    let mut mac = HmacSha256::new(key.expose());
    mac.update(DOMAIN);
    mac.update(&GRAMMAR_VERSION.to_le_bytes());
    mac.update(&HMAC_SHA256.to_le_bytes());
    mac.update(&context.format.to_le_bytes());
    mac.update(&context.isa.to_le_bytes());
    mac.update(&context.generation.to_le_bytes());
    mac.update(&context.uuid);
    mac.update(&(objects.len() as u64).to_le_bytes());
    for object in objects { update_object(&mut mac, *object, deadline)?; }
    if Instant::now() >= deadline { return Err(std::io::ErrorKind::TimedOut.into()); }
    mac.update(&[2]);
    mac.update(&manifest.len().to_le_bytes());
    let manifest_wrapper = ManifestBytes(manifest);
    update_raw(&mut mac, &manifest_wrapper, deadline)?;
    if Instant::now() >= deadline { return Err(std::io::ErrorKind::TimedOut.into()); }
    mac.update(&[255]);
    Ok(mac)
}

pub(super) fn root(
    key: &AuthorityKey, context: AuthorityContext, objects: &[&dyn SnapshotObject],
    manifest: &dyn SnapshotObject, deadline: Instant,
) -> std::io::Result<[u8; 32]> {
    Ok(root_mac(key, context, objects, manifest, deadline)?.finish())
}

pub(super) fn verify_root(
    key: &AuthorityKey, expected: &[u8], context: AuthorityContext, objects: &[&dyn SnapshotObject],
    manifest: &dyn SnapshotObject, deadline: Instant,
) -> std::io::Result<bool> {
    let mut actual = root_mac(key, context, objects, manifest, deadline)?.finish();
    let verified = constant_time::verify_slices_are_equal(&actual, expected).is_ok();
    actual.zeroize();
    Ok(verified)
}

#[cfg(test)]
fn root_and_transcript(
    key: &AuthorityKey, context: AuthorityContext, objects: &[&dyn SnapshotObject],
    manifest: &dyn SnapshotObject, deadline: Instant,
) -> std::io::Result<([u8; 32], Vec<u8>)> {
    let mac = root_mac(key, context, objects, manifest, deadline)?;
    let transcript = mac.transcript.clone();
    Ok((mac.finish(), transcript))
}

struct ManifestBytes<'a>(&'a dyn SnapshotObject);
fn update_raw(mac: &mut HmacSha256, object: &ManifestBytes<'_>, deadline: Instant) -> std::io::Result<()> {
    let mut offset = 0;
    let mut buffer = [0_u8; 64 * 1024];
    while offset < object.0.len() {
        if Instant::now() >= deadline { return Err(std::io::ErrorKind::TimedOut.into()); }
        let remaining = usize::try_from((object.0.len() - offset).min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = object.0.read_at(offset, &mut buffer[..remaining], deadline)?;
        if read == 0 || read > remaining { return Err(std::io::ErrorKind::UnexpectedEof.into()); }
        mac.update(&buffer[..read]);
        offset = offset.checked_add(read as u64).ok_or(std::io::ErrorKind::InvalidData)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Bytes(&'static [u8], &'static [u8]);
    impl SnapshotObject for Bytes {
        fn name(&self) -> &[u8] { self.0 }
        fn len(&self) -> u64 { self.1.len() as u64 }
        fn read_at(&self, offset: u64, output: &mut [u8], _: Instant) -> std::io::Result<usize> {
            let offset = usize::try_from(offset).unwrap();
            if offset >= self.1.len() { return Ok(0); }
            let count = output.len().min(self.1.len() - offset);
            output[..count].copy_from_slice(&self.1[offset..offset + count]);
            Ok(count)
        }
    }

    struct Short(&'static [u8], u64);
    impl SnapshotObject for Short {
        fn name(&self) -> &[u8] { b"short" }
        fn len(&self) -> u64 { self.1 }
        fn read_at(&self, offset: u64, output: &mut [u8], _: Instant) -> std::io::Result<usize> {
            let offset = usize::try_from(offset).unwrap();
            if offset >= self.0.len() { return Ok(0); }
            let count = output.len().min(self.0.len() - offset);
            output[..count].copy_from_slice(&self.0[offset..offset + count]);
            Ok(count)
        }
    }

    #[test]
    fn canonical_names_reject_protocol_and_recovery_aliases() {
        assert!(canonical_name(b"proc.1/pages"));
        for name in [b"".as_slice(), b"MANIFEST", b".husklet-recovery", b".husklet-recovery/report", b"/proc.1", b"proc.1/", b"a//b", b"a/../b", b"a\0b"] {
            assert!(!canonical_name(name), "accepted {name:?}");
        }
    }

    #[test]
    fn grammar_version_one_golden_hmac() {
        // Independent provenance: generated with Python 3 stdlib
        // `hmac.new(key, transcript, hashlib.sha256)`. The committed transcript
        // pins every tag, length, endian choice, ordering byte and terminator.
        const TRANSCRIPT: [u8; 146] = [
            0x68,0x75,0x73,0x6b,0x6c,0x65,0x74,0x2d,0x63,0x68,0x65,0x63,0x6b,0x70,0x6f,0x69,
            0x6e,0x74,0x2d,0x72,0x6f,0x6f,0x74,0x00,0x01,0x00,0x01,0x00,0x07,0x00,0x00,0x00,
            0x02,0x00,0x00,0x00,0x09,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x11,0x11,0x11,0x11,
            0x11,0x11,0x11,0x11,0x11,0x11,0x11,0x11,0x11,0x11,0x11,0x11,0x02,0x00,0x00,0x00,
            0x00,0x00,0x00,0x00,0x01,0x0c,0x00,0x00,0x00,0x05,0x00,0x00,0x00,0x00,0x00,0x00,
            0x00,0x70,0x72,0x6f,0x63,0x2e,0x31,0x2f,0x61,0x72,0x65,0x6e,0x61,0x61,0x72,0x65,
            0x6e,0x61,0x01,0x0c,0x00,0x00,0x00,0x05,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x70,
            0x72,0x6f,0x63,0x2e,0x31,0x2f,0x70,0x61,0x67,0x65,0x73,0x70,0x61,0x67,0x65,0x73,
            0x02,0x08,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x6d,0x61,0x6e,0x69,0x66,0x65,0x73,
            0x74,0xff,
        ];
        assert_eq!(&TRANSCRIPT[..24], DOMAIN);
        let independent = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &[0x0b; 32]), &TRANSCRIPT);
        let key = AuthorityKey::new([0x0b; 32]);
        let context = AuthorityContext { format: 7, isa: 2, generation: 9, uuid: [0x11; 16] };
        let first = Bytes(b"proc.1/arena", b"arena");
        let second = Bytes(b"proc.1/pages", b"pages");
        let manifest = Bytes(b"MANIFEST", b"manifest");
        let (actual, emitted) = root_and_transcript(&key, context, &[&first, &second], &manifest,
            Instant::now() + std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(emitted, TRANSCRIPT);
        assert_eq!(actual, [0xcc, 0xb8, 0x36, 0xcf, 0xa0, 0x3e, 0x10, 0x50, 0x2a, 0xe0, 0xea, 0x65, 0x54, 0xe1, 0xd4, 0xcc, 0xb8, 0xbc, 0xcc, 0xd5, 0x90, 0x38, 0x25, 0x58, 0x34, 0x36, 0xc2, 0x63, 0xa8, 0x17, 0xcb, 0x8c]);
        assert_eq!(independent.as_ref(), actual);
        assert!(verify_root(&key, &actual, context, &[&first, &second], &manifest,
                            Instant::now() + std::time::Duration::from_secs(1)).unwrap());
        let mut wrong = actual; wrong[0] ^= 1;
        assert!(!verify_root(&key, &wrong, context, &[&first, &second], &manifest,
                             Instant::now() + std::time::Duration::from_secs(1)).unwrap());
        let changed = AuthorityContext { generation: 10, ..context };
        assert!(!verify_root(&key, &actual, changed, &[&first, &second], &manifest,
                             Instant::now() + std::time::Duration::from_secs(1)).unwrap());
        let changed_content = Bytes(b"proc.1/pages", b"pageS");
        assert!(!verify_root(&key, &actual, context, &[&first, &changed_content], &manifest,
                             Instant::now() + std::time::Duration::from_secs(1)).unwrap());
    }

    #[test]
    fn object_order_and_short_snapshot_reads_fail_closed() {
        let key = AuthorityKey::new([1; 32]);
        let context = AuthorityContext { format: 7, isa: 1, generation: 1, uuid: [2; 16] };
        let a = Bytes(b"b", b"x");
        let b = Bytes(b"a", b"x");
        let manifest = Bytes(b"MANIFEST", b"m");
        assert!(root(&key, context, &[&a, &b], &manifest, Instant::now() + std::time::Duration::from_secs(1)).is_err());
        let short = Short(b"one", 4);
        assert!(root(&key, context, &[&short], &manifest,
                     Instant::now() + std::time::Duration::from_secs(1)).is_err());
        assert!(root(&key, context, &[], &manifest, Instant::now()).is_err());
    }

    #[test]
    fn empty_and_one_object_boundary_vectors() {
        let key = AuthorityKey::new([1; 32]);
        let context = AuthorityContext { format: 7, isa: 1, generation: 1, uuid: [2; 16] };
        let manifest = Bytes(b"MANIFEST", b"m");
        let one = Bytes(b"a", b"x");
        let deadline = || Instant::now() + std::time::Duration::from_secs(1);
        assert_eq!(root(&key, context, &[], &manifest, deadline()).unwrap(),
            [0x39,0x47,0x3b,0x5f,0x1f,0xc9,0x3f,0x64,0x96,0x61,0x72,0xaa,0x59,0xca,0xcc,0x1b,
             0xf0,0x14,0x85,0xaf,0x8a,0x50,0xcc,0x2e,0x3d,0x18,0xef,0x96,0x02,0x44,0x13,0xe1]);
        assert_eq!(root(&key, context, &[&one], &manifest, deadline()).unwrap(),
            [0xf7,0x53,0x3b,0x9f,0x40,0x3f,0x4d,0xfd,0x9f,0x19,0x7d,0x02,0xb2,0x79,0x28,0x9c,
             0x0e,0xba,0xa2,0xa3,0x99,0x14,0x00,0xa3,0xc2,0xce,0xce,0xaf,0x1d,0x0c,0xd6,0xdb]);
    }
}
