use std::{ffi::OsString, path::PathBuf, process::Command};

use super::{TargetEnvironment, TargetOs};
use crate::{BuildEnvironment, CompilerFlavor, Error, LinkerFlavor, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetTools {
    pub compiler: CompilerFlavor,
    pub linker: LinkerFlavor,
}

impl TargetTools {
    #[must_use]
    pub fn resolve(os: &TargetOs, environment: &TargetEnvironment) -> Option<Self> {
        match (os.as_str(), environment.as_str()) {
            ("macos", _) => Some(Self {
                compiler: CompilerFlavor::GnuLike,
                linker: LinkerFlavor::Darwin,
            }),
            ("windows", "msvc") => Some(Self {
                compiler: CompilerFlavor::Msvc,
                linker: LinkerFlavor::MsvcWindows,
            }),
            ("windows", _) => Some(Self {
                compiler: CompilerFlavor::GnuLike,
                linker: LinkerFlavor::GnuWindows,
            }),
            ("linux", _) => Some(Self {
                compiler: CompilerFlavor::GnuLike,
                linker: LinkerFlavor::Elf,
            }),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolCommand {
    program: PathBuf,
    arguments: Vec<OsString>,
    environment: Vec<(OsString, Option<OsString>)>,
}

impl ToolCommand {
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
            environment: Vec::new(),
        }
    }

    #[must_use]
    pub fn argument(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    #[must_use]
    pub fn environment(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.push((name.into(), Some(value.into())));
        self
    }

    fn capture(command: &Command) -> Self {
        Self {
            program: PathBuf::from(command.get_program()),
            arguments: command.get_args().map(OsString::from).collect(),
            environment: command
                .get_envs()
                .map(|(name, value)| (name.to_owned(), value.map(OsString::from)))
                .collect(),
        }
    }

    pub(crate) fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.arguments);
        for (name, value) in &self.environment {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        command
    }
}

pub struct Toolchain {
    compiler: ToolCommand,
    archiver: ToolCommand,
}

impl Toolchain {
    pub fn discover(environment: &BuildEnvironment) -> Result<Self> {
        let mut discovery = cc::Build::new();
        discovery
            .cargo_metadata(false)
            .out_dir(&environment.output)
            .target(environment.target.as_str())
            .host(environment.host.as_str());
        let compiler = discovery
            .try_get_compiler()
            .map_err(|source| Error::discovery("discover C compiler", source))?;
        let archiver = discovery
            .try_get_archiver()
            .map_err(|source| Error::discovery("discover C archiver", source))?;
        Ok(Self {
            compiler: ToolCommand::capture(&compiler.to_command()),
            archiver: ToolCommand::capture(&archiver),
        })
    }

    #[must_use]
    pub fn from_commands(compiler: ToolCommand, archiver: ToolCommand) -> Self {
        Self { compiler, archiver }
    }

    pub(crate) fn compiler_command(&self) -> Command {
        self.compiler.command()
    }

    pub(crate) fn archiver_command(&self) -> Command {
        self.archiver.command()
    }

    pub(crate) fn linker_command(&self) -> Command {
        self.compiler.command()
    }
}

#[cfg(test)]
mod tests {
    use super::{TargetTools, ToolCommand, Toolchain};
    use crate::{CompilerFlavor, LinkerFlavor, TargetEnvironment, TargetOs};
    use std::ffi::OsStr;

    #[test]
    fn complete_wrapper_commands_are_replayed_for_compile_archive_and_link() {
        let compiler = ToolCommand::new("compiler-wrapper")
            .argument("--compiler-prefix")
            .environment("COMPILER_CONTEXT", "kept");
        let archiver = ToolCommand::new("archive-wrapper")
            .argument("--archive-prefix")
            .environment("ARCHIVE_CONTEXT", "kept");
        let toolchain = Toolchain::from_commands(compiler, archiver);
        for command in [toolchain.compiler_command(), toolchain.linker_command()] {
            assert_eq!(command.get_program(), OsStr::new("compiler-wrapper"));
            assert_eq!(
                command.get_args().collect::<Vec<_>>(),
                [OsStr::new("--compiler-prefix")]
            );
            assert!(
                command
                    .get_envs()
                    .any(|(name, value)| name == "COMPILER_CONTEXT" && value == Some(OsStr::new("kept")))
            );
        }
        let command = toolchain.archiver_command();
        assert_eq!(command.get_program(), OsStr::new("archive-wrapper"));
        assert_eq!(command.get_args().collect::<Vec<_>>(), [OsStr::new("--archive-prefix")]);
        assert!(
            command
                .get_envs()
                .any(|(name, value)| name == "ARCHIVE_CONTEXT" && value == Some(OsStr::new("kept")))
        );
    }

    #[test]
    fn platform_tool_matrix_is_explicit_and_open_to_unknown_targets() {
        let cases = [
            ("linux", "gnu", CompilerFlavor::GnuLike, LinkerFlavor::Elf),
            ("macos", "", CompilerFlavor::GnuLike, LinkerFlavor::Darwin),
            ("windows", "msvc", CompilerFlavor::Msvc, LinkerFlavor::MsvcWindows),
            ("windows", "gnu", CompilerFlavor::GnuLike, LinkerFlavor::GnuWindows),
        ];
        for (os, environment, compiler, linker) in cases {
            let tools = TargetTools::resolve(&TargetOs::new(os), &TargetEnvironment::new(environment)).unwrap();
            assert_eq!(tools, TargetTools { compiler, linker });
        }
        assert!(TargetTools::resolve(&TargetOs::new("newos"), &TargetEnvironment::new("eabi")).is_none());
    }
}
