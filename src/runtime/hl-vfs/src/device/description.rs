use hl_descriptor::{ObjectError, ObjectKind, OfdMetadata, OfdTimestamp, OpenFileDescription, Readiness, SeekPosition};

use std::fmt;
use std::sync::Arc;

use crate::device::{Id, NodeKind, OpenCapability};

/// Host-selected entropy capability used by random device descriptions.
pub trait Entropy: Send + Sync {
    fn fill(&self, output: &mut [u8]) -> Result<(), ObjectError>;
}

/// Open description for a kernel-defined character device.
///
/// The description contains no host descriptor. Consequently aliases created by
/// dup or fork retain ordinary OFD identity without tying the guest device to a
/// host `/dev` namespace.
pub struct BuiltinDescription {
    kind: NodeKind,
    capability: OpenCapability,
    entropy: Option<Arc<dyn Entropy>>,
}

impl BuiltinDescription {
    pub fn open(kind: NodeKind, capability: OpenCapability) -> Result<Self, ObjectError> {
        match kind {
            NodeKind::Null | NodeKind::Zero | NodeKind::Full | NodeKind::Terminal => Ok(Self {
                kind,
                capability,
                entropy: None,
            }),
            _ => Err(ObjectError::NotSupported),
        }
    }

    pub fn random(kind: NodeKind, capability: OpenCapability, entropy: Arc<dyn Entropy>) -> Result<Self, ObjectError> {
        if !matches!(kind, NodeKind::Random | NodeKind::Urandom) {
            return Err(ObjectError::InvalidArgument);
        }
        Ok(Self {
            kind,
            capability,
            entropy: Some(entropy),
        })
    }

    const fn device(&self) -> Id {
        match self.kind {
            NodeKind::Null => Id::new(1, 3),
            NodeKind::Zero => Id::new(1, 5),
            NodeKind::Full => Id::new(1, 7),
            NodeKind::Random => Id::new(1, 8),
            NodeKind::Urandom => Id::new(1, 9),
            NodeKind::Terminal => Id::new(5, 1),
            _ => Id::new(0, 0),
        }
    }

    const fn permits_read(&self) -> bool {
        matches!(self.capability, OpenCapability::Read | OpenCapability::ReadWrite)
    }

    const fn permits_write(&self) -> bool {
        matches!(self.capability, OpenCapability::Write | OpenCapability::ReadWrite)
    }
}

impl fmt::Debug for BuiltinDescription {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BuiltinDescription")
            .field("kind", &self.kind)
            .field("capability", &self.capability)
            .finish_non_exhaustive()
    }
}

impl OpenFileDescription for BuiltinDescription {
    fn kind(&self) -> ObjectKind {
        ObjectKind::File
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        if !self.permits_read() {
            return Err(ObjectError::BadDescriptor);
        }
        match self.kind {
            NodeKind::Null => Ok(0),
            NodeKind::Zero | NodeKind::Full => {
                output.fill(0);
                Ok(output.len())
            }
            NodeKind::Random | NodeKind::Urandom => {
                self.entropy.as_ref().ok_or(ObjectError::NotSupported)?.fill(output)?;
                Ok(output.len())
            }
            NodeKind::Terminal => Err(ObjectError::WouldBlock),
            _ => Err(ObjectError::NotSupported),
        }
    }

    fn probe_read(&self, maximum: usize) -> Result<Option<usize>, ObjectError> {
        if !self.permits_read() {
            return Err(ObjectError::BadDescriptor);
        }
        Ok(match self.kind {
            NodeKind::Null => Some(0),
            NodeKind::Terminal => None,
            _ => Some(maximum),
        })
    }

    fn probe_write(&self, maximum: usize) -> Result<Option<usize>, ObjectError> {
        if !self.permits_write() {
            return Err(ObjectError::BadDescriptor);
        }
        if self.kind == NodeKind::Full {
            return Err(ObjectError::NoSpace);
        }
        Ok(matches!(self.kind, NodeKind::Null).then_some(maximum))
    }

    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        if !self.permits_write() {
            return Err(ObjectError::BadDescriptor);
        }
        if self.kind == NodeKind::Full {
            Err(ObjectError::NoSpace)
        } else {
            Ok(input.len())
        }
    }

    fn read_at(&self, _offset: u64, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.read(output)
    }

    fn write_at(&self, _offset: u64, input: &[u8]) -> Result<usize, ObjectError> {
        self.write(input)
    }

    fn seek(&self, _position: SeekPosition) -> Result<u64, ObjectError> {
        Ok(0)
    }

    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        let timestamp = OfdTimestamp {
            seconds: 0,
            nanoseconds: 0,
        };
        let device = self.device();
        Ok(OfdMetadata {
            device: 0,
            inode: (u64::from(device.major) << 32) | u64::from(device.minor),
            kind: 2,
            permissions: 0o666,
            links: 1,
            user: 0,
            group: 0,
            special_device: device.linux_encoded(),
            size: 0,
            blocks_512: 0,
            accessed: timestamp,
            modified: timestamp,
            changed: timestamp,
        })
    }

    fn readiness(&self, interests: Readiness) -> Readiness {
        let available = if self.kind == NodeKind::Terminal {
            Readiness::WRITE
        } else {
            Readiness::READ | Readiness::WRITE
        };
        Readiness::from_bits(interests.bits() & available)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedEntropy;

    impl Entropy for FixedEntropy {
        fn fill(&self, output: &mut [u8]) -> Result<(), ObjectError> {
            output.fill(0x5a);
            Ok(())
        }
    }

    #[test]
    fn null_io_metadata() {
        let readable = BuiltinDescription::open(NodeKind::Null, OpenCapability::Read).unwrap();
        let writable = BuiltinDescription::open(NodeKind::Null, OpenCapability::Write).unwrap();
        let mut output = [0xaa; 4];

        assert_eq!(readable.read(&mut output), Ok(0));
        assert_eq!(output, [0xaa; 4]);
        assert_eq!(readable.write(b"discard"), Err(ObjectError::BadDescriptor));
        assert_eq!(writable.write(b"discard"), Ok(7));
        assert_eq!(writable.probe_write(usize::MAX), Ok(Some(usize::MAX)));
        assert_eq!(writable.read(&mut output), Err(ObjectError::BadDescriptor));

        let metadata = readable.metadata().unwrap();
        assert_eq!(metadata.kind, 2);
        assert_eq!(metadata.permissions, 0o666);
        assert_eq!(metadata.special_device, Id::new(1, 3).linux_encoded());
        assert_eq!(readable.seek(SeekPosition::End(9)), Ok(0));
    }

    #[test]
    fn null_readiness() {
        let object = BuiltinDescription::open(NodeKind::Null, OpenCapability::ReadWrite).unwrap();
        let interests = Readiness::from_bits(Readiness::READ | Readiness::WRITE | Readiness::PRIORITY);

        assert_eq!(object.readiness(interests).bits(), Readiness::READ | Readiness::WRITE,);
    }

    #[test]
    fn read_probe_preserves_device_semantics() {
        let readable = BuiltinDescription::open(NodeKind::Null, OpenCapability::Read).unwrap();
        let writable = BuiltinDescription::open(NodeKind::Null, OpenCapability::Write).unwrap();
        let zero = BuiltinDescription::open(NodeKind::Zero, OpenCapability::Read).unwrap();

        assert_eq!(readable.probe_read(4), Ok(Some(0)));
        assert_eq!(zero.probe_read(4), Ok(Some(4)));
        assert_eq!(writable.probe_read(4), Err(ObjectError::BadDescriptor));
    }

    #[test]
    fn devices_preserve_semantics() {
        let zero = BuiltinDescription::open(NodeKind::Zero, OpenCapability::Read).unwrap();
        let full = BuiltinDescription::open(NodeKind::Full, OpenCapability::ReadWrite).unwrap();
        let random =
            BuiltinDescription::random(NodeKind::Urandom, OpenCapability::Read, Arc::new(FixedEntropy)).unwrap();
        let mut output = [0xaa; 4];
        assert_eq!(zero.read(&mut output), Ok(4));
        assert_eq!(output, [0; 4]);
        assert_eq!(full.write(b"x"), Err(ObjectError::NoSpace));
        assert_eq!(random.read(&mut output), Ok(4));
        assert_eq!(output, [0x5a; 4]);
        assert_eq!(random.metadata().unwrap().special_device, Id::new(1, 9).linux_encoded());
    }

    #[test]
    fn empty_terminal_would_block() {
        let terminal = BuiltinDescription::open(NodeKind::Terminal, OpenCapability::Read).unwrap();
        let mut output = [0; 1];
        assert_eq!(terminal.read(&mut output), Err(ObjectError::WouldBlock));
        assert_eq!(terminal.probe_read(output.len()), Ok(None));
        assert_eq!(terminal.metadata().unwrap().special_device, Id::new(5, 1).linux_encoded());
        assert_eq!(terminal.readiness(Readiness::from_bits(Readiness::READ)).bits(), 0);
    }
}
