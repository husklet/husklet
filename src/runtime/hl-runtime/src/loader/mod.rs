//! Executable loading and image construction during exec.

mod exec;

pub use exec::{
    ExecLoadContext, ExecutionImageBuilder, Image, Participant, PreparedLoaderExec, SourceFactory, SpaceFactory,
};

#[cfg(test)]
mod exec_test;
