//! Presentation-domain descriptors: the host surface identity and frame sequence the compositor pairs
//! a produced image with, plus the surface descriptor itself.
//!
//! Split out of [`super`], which re-exports these types.

use super::super::enums::TextureFormat;
use super::super::error::{GpuError, Result};

/// Unguessable presentation-domain identity supplied by the host surface owner.
///
/// This is distinct from [`super::id::SurfaceId`], which names a guest GPU resource only.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SurfaceToken(std::num::NonZeroU64);

impl SurfaceToken {
    pub fn new(value: u64) -> Result<Self> {
        std::num::NonZeroU64::new(value)
            .map(Self)
            .ok_or(GpuError::Invalid("surface token is zero"))
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for SurfaceToken {
    type Error = GpuError;

    fn try_from(value: u64) -> Result<Self> {
        Self::new(value)
    }
}

/// Nonzero presentation sequence used to pair a produced image with its compositor receipt.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FrameSerial(std::num::NonZeroU64);

impl FrameSerial {
    pub fn new(value: u64) -> Result<Self> {
        std::num::NonZeroU64::new(value)
            .map(Self)
            .ok_or(GpuError::Invalid("frame serial is zero"))
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for FrameSerial {
    type Error = GpuError;

    fn try_from(value: u64) -> Result<Self> {
        Self::new(value)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct SurfaceDesc {
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    /// Host presentation identity; independent from the guest resource id in `CreateSurface`.
    pub token: SurfaceToken,
}
