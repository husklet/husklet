//! Wire framing. A fixed header, a bounded payload, and no allocation before
//! the declared length has been checked.

/// Which multiplexed stream a frame belongs to.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize)]
pub struct ChannelId(u32);

impl ChannelId {
    /// Handshake, channel management, and fatal errors.
    pub const CONTROL: Self = Self(0);

    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// The host opens even channels and an extension opens odd ones, so the
    /// two sides can allocate concurrently without agreeing first.
    #[must_use]
    pub const fn is_host(self) -> bool {
        self.0.is_multiple_of(2)
    }
}

/// What a frame carries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Request,
    Response,
    Event,
    Open,
    Close,
    Reset,
    Credit,
    Ping,
    Pong,
}

impl Kind {
    const fn code(self) -> u8 {
        match self {
            Self::Request => 1,
            Self::Response => 2,
            Self::Event => 3,
            Self::Open => 4,
            Self::Close => 5,
            Self::Reset => 6,
            Self::Credit => 7,
            Self::Ping => 8,
            Self::Pong => 9,
        }
    }

    const fn decode(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Request),
            2 => Some(Self::Response),
            3 => Some(Self::Event),
            4 => Some(Self::Open),
            5 => Some(Self::Close),
            6 => Some(Self::Reset),
            7 => Some(Self::Credit),
            8 => Some(Self::Ping),
            9 => Some(Self::Pong),
            _ => None,
        }
    }

    pub const ALL: &'static [Self] = &[
        Self::Request,
        Self::Response,
        Self::Event,
        Self::Open,
        Self::Close,
        Self::Reset,
        Self::Credit,
        Self::Ping,
        Self::Pong,
    ];
}

/// Per-frame markers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Flags(u8);

impl Flags {
    /// The last frame of a multi-frame message.
    pub const END: Self = Self(0b0000_0001);
    /// The payload describes a failure rather than a result.
    pub const ERROR: Self = Self(0b0000_0010);
    /// Intermediate values were dropped; this frame supersedes them.
    pub const COALESCED: Self = Self(0b0000_0100);

    const KNOWN: u8 = 0b0000_0111;

    #[must_use]
    pub const fn none() -> Self {
        Self(0)
    }

    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn has(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    #[must_use]
    pub const fn raw(self) -> u8 {
        self.0
    }
}

/// One decoded frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    pub channel: ChannelId,
    pub kind: Kind,
    pub flags: Flags,
    pub payload: Vec<u8>,
}

impl Frame {
    /// Bytes of header preceding every payload.
    pub const HEADER: usize = 12;
    /// Largest payload accepted. A larger declared length is refused before a
    /// single byte is reserved for it.
    pub const PAYLOAD_LIMIT: usize = 1 << 20;

    #[must_use]
    pub fn new(channel: ChannelId, kind: Kind, payload: Vec<u8>) -> Self {
        Self {
            channel,
            kind,
            flags: Flags::END,
            payload,
        }
    }

    #[must_use]
    pub fn control(kind: Kind, payload: Vec<u8>) -> Self {
        Self::new(ChannelId::CONTROL, kind, payload)
    }

    #[must_use]
    pub fn flagged(mut self, flags: Flags) -> Self {
        self.flags = self.flags.with(flags);
        self
    }

    /// Serializes into the fixed header plus payload.
    ///
    /// # Errors
    /// Returns `Malformed::Oversize` when the payload exceeds the limit, so an
    /// over-large frame is refused by the sender as well as the receiver.
    pub fn encode(&self) -> Result<Vec<u8>, Malformed> {
        if self.payload.len() > Self::PAYLOAD_LIMIT {
            return Err(Malformed::Oversize {
                declared: self.payload.len(),
            });
        }
        let length = u32::try_from(self.payload.len()).map_err(|_| Malformed::Oversize {
            declared: self.payload.len(),
        })?;
        let mut bytes = Vec::with_capacity(Self::HEADER + self.payload.len());
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend_from_slice(&self.channel.raw().to_le_bytes());
        bytes.push(self.kind.code());
        bytes.push(self.flags.raw());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&self.payload);
        Ok(bytes)
    }

    /// Reads one frame from the front of `bytes`, returning it and how much was
    /// consumed. Returns `Ok(None)` when more bytes are needed.
    ///
    /// # Errors
    /// Returns `Malformed` for an oversize length, an unknown kind, unknown
    /// flag bits, or a non-zero reserved field.
    pub fn decode(bytes: &[u8]) -> Result<Option<(Self, usize)>, Malformed> {
        if bytes.len() < Self::HEADER {
            return Ok(None);
        }
        let length = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        if length > Self::PAYLOAD_LIMIT {
            return Err(Malformed::Oversize { declared: length });
        }
        let channel = ChannelId::new(u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]));
        let kind = Kind::decode(bytes[8]).ok_or(Malformed::UnknownKind(bytes[8]))?;
        if bytes[9] & !Flags::KNOWN != 0 {
            return Err(Malformed::UnknownFlags(bytes[9]));
        }
        if u16::from_le_bytes([bytes[10], bytes[11]]) != 0 {
            return Err(Malformed::Reserved);
        }
        let total = Self::HEADER + length;
        if bytes.len() < total {
            return Ok(None);
        }
        let frame = Self {
            channel,
            kind,
            flags: Flags(bytes[9]),
            payload: bytes[Self::HEADER..total].to_vec(),
        };
        Ok(Some((frame, total)))
    }
}

/// Why a byte sequence is not a frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Malformed {
    Oversize { declared: usize },
    UnknownKind(u8),
    UnknownFlags(u8),
    Reserved,
}

impl std::fmt::Display for Malformed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Oversize { declared } => write!(
                formatter,
                "frame declares {declared} bytes, above the {} byte limit",
                Frame::PAYLOAD_LIMIT
            ),
            Self::UnknownKind(code) => write!(formatter, "unknown frame kind {code}"),
            Self::UnknownFlags(bits) => write!(formatter, "unknown frame flags {bits:#010b}"),
            Self::Reserved => write!(formatter, "reserved header field is not zero"),
        }
    }
}

impl std::error::Error for Malformed {}
