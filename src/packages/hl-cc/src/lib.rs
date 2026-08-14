//! Typed, reusable C archive and shared-library construction for Cargo build scripts.

mod archive;
mod cargo;
mod library;
mod platform;

pub use archive::{
    ArchiveFormat, ArchiveSpec, CCompiler, CompilerFlavor, Definition, LanguageStandard, Sanitizer, Visibility, Warning,
};
pub use cargo::CargoDirectives;
pub use library::{LinkerFlavor, SharedLibrarySpec};
pub use platform::{BuildEnvironment, EnvFlag, EnvKey, Profile, TargetArch, TargetEnvironment, TargetOs, Triple};
