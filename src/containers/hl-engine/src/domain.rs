use std::fs::File;
use std::io::{self, Read};

const ENTROPY_SOURCE: &str = "/dev/urandom";

/// Opaque identity shared by launches in one process lifecycle domain.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Domain([u64; 2]);

impl Domain {
    /// Creates an independent process domain from host cryptographic entropy.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the host entropy source cannot be opened or
    /// does not provide a complete identity.
    pub fn new() -> Result<Self, io::Error> {
        Self::read_from(File::open(ENTROPY_SOURCE)?)
    }

    /// Returns the launch-wire identity for this domain.
    #[must_use]
    pub const fn identity(self) -> [u64; 2] {
        self.0
    }

    fn read_from(mut source: impl Read) -> Result<Self, io::Error> {
        loop {
            let mut bytes = [0_u8; 16];
            source.read_exact(&mut bytes)?;
            let identity = [
                u64::from_ne_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ]),
                u64::from_ne_bytes([
                    bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
                ]),
            ];
            if identity != [0, 0] {
                return Ok(Self(identity));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::Domain;

    #[test]
    fn public_creation_returns() {
        let domain = Domain::new().expect("create process domain");

        assert_ne!(domain.identity(), [0, 0]);
    }

    #[test]
    fn zero_entropy_block() {
        let mut bytes = [0_u8; 32];
        bytes[16] = 7;

        let domain = Domain::read_from(Cursor::new(bytes)).expect("read the first nonzero identity");

        assert_ne!(domain.identity(), [0, 0]);
        assert_eq!(domain.identity()[0].to_ne_bytes()[0], 7);
    }
}
