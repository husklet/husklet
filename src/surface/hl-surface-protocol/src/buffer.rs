//! Versioned metadata carried by a Husklet host-external dma-buf.

pub const DRM_FMT_XRGB8888: u32 = 0x3432_5258;
pub const DRM_FMT_ARGB8888: u32 = 0x3432_5241;

/// Husklet-private DRM modifier for a host-external image.
///
/// The plane fd carries pixels from offset zero followed by a versioned [`Header`]. Pixel storage is owned
/// by the host GPU and must never be CPU-imported from that fd. The private vendor byte keeps this distinct
/// from every standardized hardware layout; GBM, EGL, and the compositor must recognize the exact value.
pub const MODIFIER: u64 = 0x7f48_4c45_5854_0002;
pub const VERSION: u16 = 2;
pub const HEADER_LEN: usize = 64;
pub const PLANE_OFFSET: u64 = 0;

/// Eight-byte signature written at the beginning of the trailing external-image header.
///
/// Build scripts use this value to generate matching C declarations for guest shims.
pub const MAGIC: [u8; 8] = *b"HLEXTBUF";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Truncated,
    Magic,
    Version,
    Length,
    Flags,
    Token,
    Serial,
    Dimensions,
    Format,
    RowOverflow,
    Stride,
    Offset,
    SpanOverflow,
    Allocation,
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Truncated => "external image header is truncated",
            Self::Magic => "external image magic is invalid",
            Self::Version => "external image version is unsupported",
            Self::Length => "external image header length is invalid",
            Self::Flags => "external image flags are unsupported",
            Self::Token => "external image token is zero",
            Self::Serial => "external image serial is zero",
            Self::Dimensions => "external image dimensions are zero",
            Self::Format => "external image format is unsupported",
            Self::RowOverflow => "external image row size overflows",
            Self::Stride => "external image stride is too small",
            Self::Offset => "external image plane offset is invalid",
            Self::SpanOverflow => "external image span overflows",
            Self::Allocation => "external image allocation is too small",
        })
    }
}

impl std::error::Error for Error {}

/// Stable metadata correlating a guest dma-buf with a host-owned image.
///
/// This is an explicitly little-endian byte protocol, not a shared Rust/C struct layout. `token` identifies
/// one allocation for the compositor session and `serial` identifies its most recently completed GPU
/// publication. A zero serial means the allocation has not produced a frame yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Header {
    pub token: u64,
    pub serial: u64,
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub stride: u32,
    pub plane_offset: u64,
    pub allocation_size: u64,
}

impl Header {
    pub fn new(
        token: u64,
        width: u32,
        height: u32,
        fourcc: u32,
        stride: u32,
        allocation_size: u64,
    ) -> Result<Self, Error> {
        let header = Self {
            token,
            serial: 0,
            width,
            height,
            fourcc,
            stride,
            plane_offset: PLANE_OFFSET,
            allocation_size,
        };
        header.validate()?;
        Ok(header)
    }

    pub fn with_serial(mut self, serial: u64) -> Result<Self, Error> {
        if serial == 0 {
            return Err(Error::Serial);
        }
        self.serial = serial;
        Ok(self)
    }

    pub fn plane_len(self) -> Result<u64, Error> {
        u64::from(self.height)
            .checked_mul(u64::from(self.stride))
            .ok_or(Error::SpanOverflow)
    }

    pub fn header_offset(self) -> Result<u64, Error> {
        self.plane_offset
            .checked_add(self.plane_len()?)
            .ok_or(Error::SpanOverflow)
    }

    pub fn encode(self) -> Result<[u8; HEADER_LEN], Error> {
        self.validate()?;
        let mut bytes = [0; HEADER_LEN];
        bytes[0..8].copy_from_slice(&MAGIC);
        bytes[8..10].copy_from_slice(&VERSION.to_le_bytes());
        bytes[10..12].copy_from_slice(&(HEADER_LEN as u16).to_le_bytes());
        bytes[16..24].copy_from_slice(&self.token.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.serial.to_le_bytes());
        bytes[32..36].copy_from_slice(&self.width.to_le_bytes());
        bytes[36..40].copy_from_slice(&self.height.to_le_bytes());
        bytes[40..44].copy_from_slice(&self.fourcc.to_le_bytes());
        bytes[44..48].copy_from_slice(&self.stride.to_le_bytes());
        bytes[48..56].copy_from_slice(&self.plane_offset.to_le_bytes());
        bytes[56..64].copy_from_slice(&self.allocation_size.to_le_bytes());
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < HEADER_LEN {
            return Err(Error::Truncated);
        }
        if bytes[0..8] != MAGIC {
            return Err(Error::Magic);
        }
        if u16::from_le_bytes(bytes[8..10].try_into().unwrap()) != VERSION {
            return Err(Error::Version);
        }
        if usize::from(u16::from_le_bytes(bytes[10..12].try_into().unwrap())) != HEADER_LEN {
            return Err(Error::Length);
        }
        if bytes[12..16] != [0; 4] {
            return Err(Error::Flags);
        }
        let header = Self {
            token: u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
            serial: u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
            width: u32::from_le_bytes(bytes[32..36].try_into().unwrap()),
            height: u32::from_le_bytes(bytes[36..40].try_into().unwrap()),
            fourcc: u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
            stride: u32::from_le_bytes(bytes[44..48].try_into().unwrap()),
            plane_offset: u64::from_le_bytes(bytes[48..56].try_into().unwrap()),
            allocation_size: u64::from_le_bytes(bytes[56..64].try_into().unwrap()),
        };
        header.validate()?;
        Ok(header)
    }

    fn validate(self) -> Result<(), Error> {
        if self.token == 0 {
            return Err(Error::Token);
        }
        if self.width == 0 || self.height == 0 {
            return Err(Error::Dimensions);
        }
        if !matches!(self.fourcc, DRM_FMT_ARGB8888 | DRM_FMT_XRGB8888) {
            return Err(Error::Format);
        }
        let row = u64::from(self.width)
            .checked_mul(4)
            .ok_or(Error::RowOverflow)?;
        if u64::from(self.stride) < row {
            return Err(Error::Stride);
        }
        if self.plane_offset != PLANE_OFFSET {
            return Err(Error::Offset);
        }
        let header_offset = self.header_offset()?;
        if header_offset.checked_add(HEADER_LEN as u64) != Some(self.allocation_size) {
            return Err(Error::Allocation);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrips_exactly() {
        let header = Header::new(
            9,
            1728,
            1117,
            DRM_FMT_ARGB8888,
            6912,
            6912 * 1117 + HEADER_LEN as u64,
        )
        .unwrap()
        .with_serial(17)
        .unwrap();
        assert_eq!(Header::decode(&header.encode().unwrap()).unwrap(), header);
    }

    #[test]
    fn header_rejects_hostile_geometry_and_metadata() {
        let valid = Header::new(1, 3, 2, DRM_FMT_XRGB8888, 16, 32 + HEADER_LEN as u64)
            .unwrap()
            .encode()
            .unwrap();

        let mutations: [fn(&mut [u8; HEADER_LEN]); 5] = [
            |bytes| bytes[0] ^= 1,
            |bytes| bytes[8] ^= 1,
            |bytes| bytes[16..24].fill(0),
            |bytes| bytes[44..48].fill(0),
            |bytes| bytes[56..64].fill(0),
        ];
        for mutate in mutations {
            let mut bytes = valid;
            mutate(&mut bytes);
            assert!(Header::decode(&bytes).is_err());
        }
        assert!(Header::decode(&valid[..HEADER_LEN - 1]).is_err());
    }

    #[test]
    fn header_rejects_noncanonical_plane_offset() {
        let mut bytes = Header::new(1, 3, 2, DRM_FMT_XRGB8888, 16, 32 + HEADER_LEN as u64)
            .unwrap()
            .encode()
            .unwrap();
        bytes[48..56].copy_from_slice(&1u64.to_le_bytes());
        bytes[56..64].copy_from_slice(&(33 + HEADER_LEN as u64).to_le_bytes());

        assert_eq!(Header::decode(&bytes), Err(Error::Offset));
    }

    #[test]
    fn header_requires_exact_canonical_allocation() {
        let exact = 16 * 2 + HEADER_LEN as u64;
        assert!(Header::new(1, 3, 2, DRM_FMT_XRGB8888, 16, exact).is_ok());
        assert_eq!(
            Header::new(1, 3, 2, DRM_FMT_XRGB8888, 16, exact - 1),
            Err(Error::Allocation)
        );
        assert_eq!(
            Header::new(1, 3, 2, DRM_FMT_XRGB8888, 16, exact + 1),
            Err(Error::Allocation)
        );
    }
}
