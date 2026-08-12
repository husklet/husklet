//! Language-neutral rules for repository structure and manifests.

mod catchall;
mod dependency;
mod documentation;
mod empty;
mod escape;
mod folder;
mod length;
mod ownership;
mod shape;
mod suite;
mod suite_path;

pub use catchall::SourcePath as CatchAllSourcePath;
pub use dependency::Direction as DependencyDirection;
pub use documentation::Documentation;
pub use empty::Directory as EmptyDirectory;
pub use escape::Repository as RepositoryEscape;
pub use folder::SingleFileDirectory;
pub use length::FileLength;
pub use ownership::RuntimeTool;
pub use shape::{FileName, FolderNoun, ModulePrefix, ParentName, PrefixDirectory, TestName};
pub use suite::{Dependency as TestDependency, Directory as TestDirectory};
pub use suite_path::KebabPath as TestSuiteKebabPath;
