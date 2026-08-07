//! Loader request, diagnostic, and result value types.

use crate::{
    AddressSpaceError, DynamicLoaderHandoff, GuestCredentials, GuestFeatures, ImageProjection, ImageRole,
    ImageSourceError, InitialStack, InitialTlsPlan, InspectError, ProtectionPlanError, ReservedMapping, StackError,
    TlsPlanError,
};
use hl_isa::GuestArchitecture;
use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoaderPhase {
    MainRead,
    MainInspect,
    InterpreterPrepare,
    MainStage,
    InterpreterStage,
    StackPlan,
    Commit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoaderDiagnostic {
    pub phase: LoaderPhase,
    pub elapsed_us: u64,
}

/// Consumer-owned destination for optional loader phase diagnostics.
///
/// Implementations must return promptly. Diagnostics are observational and a
/// destination cannot alter or fail an image transaction.
pub trait LoaderDiagnostics: Send + Sync {
    fn try_publish(&self, diagnostic: LoaderDiagnostic);
}

/// Complete host-neutral input to one process-image transaction.
pub struct LoadRequest<'a> {
    pub architecture: GuestArchitecture,
    /// Object read as the main ELF image. This differs from
    /// `executable_path` when Linux resolves a `#!` script.
    pub image_path: &'a [u8],
    /// Original exec filename projected through `AT_EXECFN`.
    pub executable_path: &'a [u8],
    pub arguments: &'a [&'a [u8]],
    pub environment: &'a [&'a [u8]],
    pub random: [u8; 16],
    pub credentials: GuestCredentials,
    pub features: GuestFeatures,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadError {
    Source { role: ImageRole, error: ImageSourceError },
    Inspect { role: ImageRole, error: InspectError },
    AddressSpace(AddressSpaceError),
    Stack(StackError),
    InvalidReservation,
    InvalidInterpreter,
    Tls(TlsPlanError),
    Protection(ProtectionPlanError),
}

impl fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "load transaction failed: {self:?}")
    }
}

impl Error for LoadError {}

/// Published mapping coordinates, without adapter reservation identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoadedMapping {
    pub(super) address: u64,
    pub(super) size: u64,
}

impl LoadedMapping {
    pub(crate) const fn from_reserved<R>(mapping: &ReservedMapping<R>) -> Self {
        Self {
            address: mapping.address(),
            size: mapping.size(),
        }
    }

    #[must_use]
    pub const fn address(self) -> u64 {
        self.address
    }

    #[must_use]
    pub const fn size(self) -> u64 {
        self.size
    }
}

/// Result returned only after every mapping becomes visible atomically.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedProcess {
    pub(super) main: LoadedMapping,
    pub(super) interpreter: Option<LoadedMapping>,
    pub(super) stack_mapping: LoadedMapping,
    pub(super) usable_stack: LoadedMapping,
    pub(super) stack_overread: Option<LoadedMapping>,
    pub(super) initial_stack: InitialStack,
    pub(super) tls: InitialTlsPlan,
    pub(super) dynamic_handoff: DynamicLoaderHandoff,
    pub(super) main_projection: Option<ImageProjection>,
}

impl LoadedProcess {
    #[must_use]
    pub const fn main(&self) -> LoadedMapping {
        self.main
    }

    #[must_use]
    pub const fn interpreter(&self) -> Option<LoadedMapping> {
        self.interpreter
    }

    #[must_use]
    pub const fn stack_mapping(&self) -> LoadedMapping {
        self.stack_mapping
    }

    /// Guest-visible writable main-stack interval, excluding its lower guard.
    #[must_use]
    pub const fn usable_stack(&self) -> LoadedMapping {
        self.usable_stack
    }

    /// Writable cushion above the logical stack top on x86-64.
    #[must_use]
    pub const fn stack_overread(&self) -> Option<LoadedMapping> {
        self.stack_overread
    }

    #[must_use]
    pub const fn initial_stack(&self) -> &InitialStack {
        &self.initial_stack
    }

    #[must_use]
    pub const fn initial_tls(&self) -> &InitialTlsPlan {
        &self.tls
    }

    #[must_use]
    pub const fn dynamic_handoff(&self) -> &DynamicLoaderHandoff {
        &self.dynamic_handoff
    }

    #[must_use]
    pub const fn main_projection(&self) -> Option<ImageProjection> {
        self.main_projection
    }
}

impl From<AddressSpaceError> for LoadError {
    fn from(error: AddressSpaceError) -> Self {
        Self::AddressSpace(error)
    }
}

impl From<StackError> for LoadError {
    fn from(error: StackError) -> Self {
        Self::Stack(error)
    }
}
