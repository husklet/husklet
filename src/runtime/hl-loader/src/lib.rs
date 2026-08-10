//! Bounded ELF validation and host-neutral image planning.
//!
//! This foundation parses owned or borrowed image bytes into immutable values.
//! It deliberately does not read paths, map host memory, populate an address
//! space, build the initial stack, or invoke guest execution.

#![forbid(unsafe_code)]

mod dynamic;
mod elf;
mod handoff;
mod inspection_error;
mod load_policy;
mod main_image;
mod mapping_transaction;
mod model;
mod port;
mod projection;
mod protection;
mod stack;
mod stack_layout;
mod tls;
mod transaction;

pub use dynamic::{DynamicEntry, DynamicTable};
pub use elf::ElfInspector;
pub use handoff::{DynamicLoaderHandoff, LoadedModuleHandoff};
pub use inspection_error::{ImageLimits, InspectError};
pub use load_policy::{ExecutablePlacement, LoadLimits};
pub use main_image::{ImageReadAt, MainImageInspectError, MainImageInspector, MainImageMetadata};
pub use model::{
    FileRegion, ImageKind, ImagePlan, InterpreterPath, LoadSegment, ProgramHeaderTable, RelocationWrite, RelroRegion,
    SegmentFlags, TlsTemplate,
};
pub use port::{
    AddressSpaceError, ImageProtectionRegistry, ImageRole, ImageSource, ImageSourceError, MappingKind,
    MappingPlacement, Protection, ReservedMapping, TransactionalAddressSpace,
};
pub use projection::{GuestAddressRange, ImageProjection};
pub use protection::{
    GuestProtectionPlan, GuestProtectionRange, ImageProtectionPlan, ProtectionPlanError, ProtectionRange,
};
pub use stack::{
    AuxiliaryEntry, AuxiliaryType, GuestCredentials, GuestFeatures, InitialStack, StackError, StackLimits,
    StackPlanner, StackRequest,
};
pub use tls::{InitialTlsPlan, ThreadLocalStorage, TlsModulePlacement, TlsModuleRequest, TlsPlanError, TlsVariant};
pub use transaction::{
    LoadError, LoadRequest, LoadedMapping, LoadedProcess, Loader, LoaderDiagnostic, LoaderDiagnostics, LoaderPhase,
};

#[cfg(test)]
mod dynamic_test;
#[cfg(test)]
mod elf_test;
#[cfg(test)]
mod image_test;
#[cfg(test)]
mod protection_test;
#[cfg(test)]
mod registry_test;
#[cfg(test)]
mod stack_test;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tls_test;
#[cfg(test)]
mod transaction_test;
