use std::{path::PathBuf, process::Command};

use crate::BuildEnvironment;

pub struct Toolchain {
    compiler: cc::Tool,
    archiver: PathBuf,
}

impl Toolchain {
    pub fn discover(environment: &BuildEnvironment) -> Result<Self, String> {
        let mut discovery = cc::Build::new();
        discovery
            .cargo_metadata(false)
            .out_dir(&environment.output)
            .target(environment.target.as_str())
            .host(environment.host.as_str());
        let compiler = discovery
            .try_get_compiler()
            .map_err(|error| format!("discover C compiler: {error}"))?;
        let archiver = discovery
            .try_get_archiver()
            .map_err(|error| format!("discover C archiver: {error}"))?;
        Ok(Self {
            compiler,
            archiver: PathBuf::from(archiver.get_program()),
        })
    }

    pub(crate) fn configure(&self, build: &mut cc::Build) {
        build.compiler(self.compiler.path()).archiver(&self.archiver);
    }

    pub(crate) fn linker_command(&self) -> Command {
        self.compiler.to_command()
    }
}
