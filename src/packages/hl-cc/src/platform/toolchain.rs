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

    #[must_use]
    fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
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
        let discovered_archiver = discovery
            .try_get_archiver()
            .map_err(|source| Error::discovery("discover C archiver", source))?;
        let archiver = select_archiver(environment, ToolCommand::capture(&discovered_archiver));
        Ok(Self {
            compiler: ToolCommand::capture(&compiler.to_command()),
            archiver,
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

fn select_archiver(environment: &BuildEnvironment, discovered: ToolCommand) -> ToolCommand {
    if uses_apple_system_archiver(environment, &discovered) {
        // A Cargo shell can export AR for an unrelated guest toolchain. In
        // particular, a Darwin shell may also carry a Linux cross-ar for guest
        // binaries. Apple ld does not reliably force-load the GNU archive
        // dialect that tool emits, even when every member is a valid Mach-O
        // object. Native Darwin artifacts therefore use the platform archiver;
        // target-specific overrides remain available for deliberate toolchains.
        discovered.with_program("/usr/bin/ar")
    } else {
        discovered
    }
}

fn uses_apple_system_archiver(environment: &BuildEnvironment, discovered: &ToolCommand) -> bool {
    environment.target_os.as_str() == "macos"
        && environment.host.as_str().ends_with("-apple-darwin")
        && selected_archiver_variable(environment).is_some_and(|(name, _)| name == "AR")
        && is_linux_guest_archiver(&discovered.program)
}

fn is_linux_guest_archiver(program: &std::path::Path) -> bool {
    let name = program
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default();
    name.ends_with("-ar") && name.contains("-linux-")
}

fn selected_archiver_variable(environment: &BuildEnvironment) -> Option<(String, &std::ffi::OsStr)> {
    let target = environment.target.as_str();
    let underscored = target.replace(['-', '.'], "_");
    let scope = if environment.host == environment.target {
        "HOST_AR"
    } else {
        "TARGET_AR"
    };
    [
        format!("AR_{target}"),
        format!("AR_{underscored}"),
        scope.to_owned(),
        "AR".to_owned(),
    ]
    .into_iter()
    .find_map(|name| environment.variable(&name).map(|value| (name, value)))
}

#[cfg(test)]
mod tests {
    use super::{TargetTools, ToolCommand, Toolchain, select_archiver, selected_archiver_variable};
    use crate::{BuildEnvironment, CompilerFlavor, LinkerFlavor, TargetEnvironment, TargetOs};
    use std::{
        collections::BTreeMap,
        ffi::{OsStr, OsString},
    };

    fn environment(host: &str, target: &str, target_os: &str, extra: &[(&str, &str)]) -> BuildEnvironment {
        let mut variables = BTreeMap::from([
            (OsString::from("HOST"), OsString::from(host)),
            (OsString::from("TARGET"), OsString::from(target)),
            (OsString::from("CARGO_CFG_TARGET_OS"), OsString::from(target_os)),
            (OsString::from("CARGO_CFG_TARGET_ARCH"), OsString::from("aarch64")),
            (OsString::from("CARGO_CFG_TARGET_ENV"), OsString::new()),
            (OsString::from("OUT_DIR"), OsString::from("out")),
            (OsString::from("CARGO_MANIFEST_DIR"), OsString::from("crate")),
            (OsString::from("PROFILE"), OsString::from("debug")),
        ]);
        variables.extend(
            extra
                .iter()
                .map(|(name, value)| (OsString::from(name), OsString::from(value))),
        );
        BuildEnvironment::from_variables(variables).unwrap()
    }

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

    #[test]
    fn native_darwin_rejects_unscoped_guest_archiver_but_honors_target_override() {
        let polluted = environment(
            "aarch64-apple-darwin",
            "aarch64-apple-darwin",
            "macos",
            &[("AR", "x86_64-unknown-linux-gnu-ar")],
        );
        assert_eq!(selected_archiver_variable(&polluted).unwrap().0, "AR");
        let selected = select_archiver(
            &polluted,
            ToolCommand::new("x86_64-unknown-linux-gnu-ar")
                .argument("--wrapper-prefix")
                .argument("--arflags-value")
                .environment("ARCHIVER_CONTEXT", "kept"),
        );
        assert_eq!(selected.command().get_program(), OsStr::new("/usr/bin/ar"));
        assert_eq!(
            selected.command().get_args().collect::<Vec<_>>(),
            [OsStr::new("--wrapper-prefix"), OsStr::new("--arflags-value")]
        );
        assert!(
            selected
                .command()
                .get_envs()
                .any(|(name, value)| name == "ARCHIVER_CONTEXT" && value == Some(OsStr::new("kept")))
        );

        let explicit = environment(
            "aarch64-apple-darwin",
            "aarch64-apple-darwin",
            "macos",
            &[("AR_aarch64_apple_darwin", "custom-darwin-ar")],
        );
        assert_eq!(
            selected_archiver_variable(&explicit).unwrap().0,
            "AR_aarch64_apple_darwin"
        );
        let selected = select_archiver(&explicit, ToolCommand::new("custom-darwin-ar"));
        assert_eq!(selected.command().get_program(), OsStr::new("custom-darwin-ar"));

        let cross_host = environment("x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "macos", &[]);
        let selected = select_archiver(&cross_host, ToolCommand::new("llvm-ar"));
        assert_eq!(selected.command().get_program(), OsStr::new("llvm-ar"));
    }

    #[test]
    fn archiver_environment_precedence_matches_cc_for_native_and_cross_targets() {
        let native = environment(
            "aarch64-apple-darwin",
            "aarch64-apple-darwin",
            "macos",
            &[
                ("AR", "global"),
                ("TARGET_AR", "irrelevant-cross"),
                ("HOST_AR", "native"),
                ("AR_aarch64_apple_darwin", "normalized-specific"),
                ("AR_aarch64-apple-darwin", "exact-specific"),
            ],
        );
        let (name, value) = selected_archiver_variable(&native).unwrap();
        assert_eq!(name, "AR_aarch64-apple-darwin");
        assert_eq!(value, OsStr::new("exact-specific"));

        let native_host = environment(
            "aarch64-apple-darwin",
            "aarch64-apple-darwin",
            "macos",
            &[
                ("AR", "global"),
                ("TARGET_AR", "irrelevant-cross"),
                ("HOST_AR", "native"),
            ],
        );
        assert_eq!(selected_archiver_variable(&native_host).unwrap().0, "HOST_AR");

        let cross = environment(
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "macos",
            &[
                ("AR", "global"),
                ("HOST_AR", "irrelevant-native"),
                ("TARGET_AR", "cross"),
            ],
        );
        assert_eq!(selected_archiver_variable(&cross).unwrap().0, "TARGET_AR");

        let dotted = environment(
            "aarch64-apple-darwin",
            "thumbv8m.main-none-eabi",
            "macos",
            &[
                ("AR_thumbv8m_main_none_eabi", "dotted-specific"),
                ("TARGET_AR", "cross"),
            ],
        );
        assert_eq!(
            selected_archiver_variable(&dotted).unwrap().0,
            "AR_thumbv8m_main_none_eabi"
        );
    }

    #[test]
    fn empty_higher_priority_archiver_suppresses_lower_priority_values_like_cc() {
        for (name, value) in [
            ("AR_aarch64-apple-darwin", ""),
            ("AR_aarch64_apple_darwin", "  "),
            ("HOST_AR", ""),
        ] {
            let native = environment(
                "aarch64-apple-darwin",
                "aarch64-apple-darwin",
                "macos",
                &[("AR", "x86_64-unknown-linux-gnu-ar"), (name, value)],
            );
            let (selected_name, selected_value) = selected_archiver_variable(&native).unwrap();
            assert_eq!(selected_name, name);
            assert_eq!(selected_value, OsStr::new(value));
            let selected = select_archiver(&native, ToolCommand::new("cc-default-ar"));
            assert_eq!(selected.command().get_program(), OsStr::new("cc-default-ar"));
        }
    }

    #[test]
    fn valid_unscoped_darwin_archivers_remain_byte_for_byte_unchanged() {
        for value in ["llvm-ar", "/custom/toolchain/bin/ar", "sccache llvm-ar --plugin cache"] {
            let environment = environment(
                "aarch64-apple-darwin",
                "aarch64-apple-darwin",
                "macos",
                &[("AR", value)],
            );
            let discovered = ToolCommand::new(value)
                .argument("--arflags")
                .environment("ARCHIVER_CONTEXT", "preserved");
            assert_eq!(select_archiver(&environment, discovered.clone()), discovered);
        }
    }

    #[test]
    fn wrapper_arguments_and_flags_cannot_impersonate_the_selected_archiver() {
        let environment = environment(
            "aarch64-apple-darwin",
            "aarch64-apple-darwin",
            "macos",
            &[("AR", "custom-wrapper")],
        );
        for argument in [
            "/toolchain/bin/x86_64-unknown-linux-gnu-ar",
            "--plugin=/toolchain/x86_64-unknown-linux-gnu-ar",
            "--label=contains-linux-tool-ar",
        ] {
            let discovered = ToolCommand::new("custom-wrapper")
                .argument(argument)
                .argument("--arflags")
                .environment("ARCHIVER_CONTEXT", "preserved");
            assert_eq!(select_archiver(&environment, discovered.clone()), discovered);
        }
    }

    #[test]
    fn apple_cross_build_rejects_only_a_linux_guest_archiver_from_global_ar() {
        let environment = environment(
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "macos",
            &[("AR", "/nix/store/tool/bin/x86_64-unknown-linux-gnu-ar")],
        );
        let selected = select_archiver(
            &environment,
            ToolCommand::new("/nix/store/tool/bin/x86_64-unknown-linux-gnu-ar")
                .argument("--arflags")
                .environment("ARCHIVER_CONTEXT", "preserved"),
        );
        let command = selected.command();
        assert_eq!(command.get_program(), OsStr::new("/usr/bin/ar"));
        assert_eq!(command.get_args().collect::<Vec<_>>(), [OsStr::new("--arflags")]);
        assert!(
            command
                .get_envs()
                .any(|(name, value)| name == "ARCHIVER_CONTEXT" && value == Some(OsStr::new("preserved")))
        );
    }
}
