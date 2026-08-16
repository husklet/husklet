//! Typed, reusable C archive and shared-library construction for Cargo build scripts.

mod archive;
mod cargo;
mod error;
mod library;
mod platform;

pub use archive::{
    Archive, ArchiveFormat, ArchiveSpec, CCompiler, CompilerFlavor, Definition, LanguageStandard, Sanitizer,
    Visibility, Warning,
};
pub use cargo::CargoDirectives;
pub use error::{Error, Result};
pub use library::{LinkerFlavor, SharedLibrarySpec};
pub use platform::{
    BuildEnvironment, EnvFlag, EnvKey, Profile, TargetArch, TargetEnvironment, TargetOs, TargetTools, ToolCommand,
    Toolchain, Triple,
};
