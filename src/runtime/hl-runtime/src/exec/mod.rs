//! Cross-domain exec coordination and VFS image sources.

mod runtime;
mod source;

pub use runtime::{Role, Runtime, RuntimeDependencies, RuntimeDependenciesBuilder};
pub use source::{CurrentDescriptorTable, VfsImageSource, VfsSourceFactory};

#[cfg(test)]
mod runtime_test;
#[cfg(test)]
mod source_test;
