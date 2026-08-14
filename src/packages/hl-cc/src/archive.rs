use std::path::PathBuf;

use crate::{BuildEnvironment, Error, Result, Toolchain};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Definition {
    name: String,
    value: Option<String>,
}

impl Definition {
    #[must_use]
    pub fn flag(name: impl Into<String>) -> Self {
        Self::new(name.into(), None)
    }

    #[must_use]
    pub fn value(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self::new(name.into(), Some(value.into()))
    }

    fn new(name: String, value: Option<String>) -> Self {
        assert!(valid_identifier(&name), "invalid C definition name {name:?}");
        Self { name, value }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn replacement(&self) -> Option<&str> {
        self.value.as_deref()
    }
}

fn valid_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerFlavor {
    GnuLike,
    Msvc,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveFormat {
    Native,
    Darwin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LanguageStandard {
    C11,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Visibility {
    Default,
    Hidden,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sanitizer {
    Address,
    Leak,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Warning {
    All,
    Extra,
    Pedantic,
    Conversion,
    Shadow,
    StrictPrototypes,
    MissingPrototypes,
    ImplicitFunctionDeclarationError,
    ImplicitIntError,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Archive {
    path: PathBuf,
}

impl Archive {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Warning {
    const fn flag(self) -> &'static str {
        match self {
            Self::All => "-Wall",
            Self::Extra => "-Wextra",
            Self::Pedantic => "-Wpedantic",
            Self::Conversion => "-Wconversion",
            Self::Shadow => "-Wshadow",
            Self::StrictPrototypes => "-Wstrict-prototypes",
            Self::MissingPrototypes => "-Wmissing-prototypes",
            Self::ImplicitFunctionDeclarationError => "-Werror=implicit-function-declaration",
            Self::ImplicitIntError => "-Werror=implicit-int",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[must_use]
pub struct ArchiveSpec {
    pub name: String,
    pub sources: Vec<PathBuf>,
    pub includes: Vec<PathBuf>,
    pub definitions: Vec<Definition>,
    pub forced_include: Option<PathBuf>,
    pub language: Option<LanguageStandard>,
    pub optimization: Option<u32>,
    pub debug: Option<bool>,
    pub pic: Option<bool>,
    pub visibility: Option<Visibility>,
    pub function_sections: Option<bool>,
    pub data_sections: Option<bool>,
    pub warnings_enabled: Option<bool>,
    pub warnings: Vec<Warning>,
    pub sanitizer: Option<Sanitizer>,
    pub archive_format: ArchiveFormat,
    pub cargo_metadata: Option<bool>,
    pub omit_frame_pointer: Option<bool>,
}

impl ArchiveSpec {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            sources: Vec::new(),
            includes: Vec::new(),
            definitions: Vec::new(),
            forced_include: None,
            language: None,
            optimization: None,
            debug: None,
            pic: None,
            visibility: None,
            function_sections: None,
            data_sections: None,
            warnings_enabled: None,
            warnings: Vec::new(),
            sanitizer: None,
            archive_format: ArchiveFormat::Native,
            cargo_metadata: None,
            omit_frame_pointer: None,
        }
    }
    pub fn sources(mut self, values: impl IntoIterator<Item = impl Into<PathBuf>>) -> Self {
        self.sources.extend(values.into_iter().map(Into::into));
        self
    }
    pub fn includes(mut self, values: impl IntoIterator<Item = impl Into<PathBuf>>) -> Self {
        self.includes.extend(values.into_iter().map(Into::into));
        self
    }
    pub fn definitions(mut self, values: impl IntoIterator<Item = Definition>) -> Self {
        self.definitions.extend(values);
        self
    }
    pub fn forced_include(mut self, value: impl Into<PathBuf>) -> Self {
        self.forced_include = Some(value.into());
        self
    }
    pub const fn language(mut self, value: LanguageStandard) -> Self {
        self.language = Some(value);
        self
    }
    pub const fn optimization(mut self, value: u32) -> Self {
        self.optimization = Some(value);
        self
    }
    pub const fn debug(mut self, value: bool) -> Self {
        self.debug = Some(value);
        self
    }
    pub const fn pic(mut self, value: bool) -> Self {
        self.pic = Some(value);
        self
    }
    pub const fn visibility(mut self, value: Visibility) -> Self {
        self.visibility = Some(value);
        self
    }
    pub const fn function_sections(mut self, value: bool) -> Self {
        self.function_sections = Some(value);
        self
    }
    pub const fn data_sections(mut self, value: bool) -> Self {
        self.data_sections = Some(value);
        self
    }
    pub const fn warnings_enabled(mut self, value: bool) -> Self {
        self.warnings_enabled = Some(value);
        self
    }
    pub fn warnings(mut self, values: impl IntoIterator<Item = Warning>) -> Self {
        self.warnings.extend(values);
        self
    }
    pub const fn sanitizer(mut self, value: Sanitizer) -> Self {
        self.sanitizer = Some(value);
        self
    }
    pub const fn archive_format(mut self, value: ArchiveFormat) -> Self {
        self.archive_format = value;
        self
    }
    pub const fn cargo_metadata(mut self, value: bool) -> Self {
        self.cargo_metadata = Some(value);
        self
    }
    pub const fn omit_frame_pointer(mut self, value: bool) -> Self {
        self.omit_frame_pointer = Some(value);
        self
    }
}

#[must_use]
pub struct CCompiler<'a> {
    environment: &'a BuildEnvironment,
    toolchain: &'a Toolchain,
    flavor: CompilerFlavor,
}

impl<'a> CCompiler<'a> {
    pub const fn new(environment: &'a BuildEnvironment, toolchain: &'a Toolchain, flavor: CompilerFlavor) -> Self {
        Self {
            environment,
            toolchain,
            flavor,
        }
    }
    pub fn archive(&self, spec: &ArchiveSpec) -> Result<Archive> {
        let mut objects = Vec::with_capacity(spec.sources.len());
        for (index, source) in spec.sources.iter().enumerate() {
            let extension = if self.flavor == CompilerFlavor::Msvc {
                "obj"
            } else {
                "o"
            };
            let object = self
                .environment
                .output
                .join(format!("{}-{index}.{extension}", spec.name));
            let mut command = self.toolchain.compiler_command();
            configure_compiler(&mut command, self.flavor, spec);
            match self.flavor {
                CompilerFlavor::GnuLike => {
                    command.arg("-c").arg(source).arg("-o").arg(&object);
                }
                CompilerFlavor::Msvc => {
                    command.arg("/c").arg(source).arg(format!("/Fo{}", object.display()));
                }
            }
            run(&mut command, "compile C source", source)?;
            objects.push(object);
        }
        let filename = format!("lib{}.a", spec.name);
        let path = self.environment.output.join(filename);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|source| Error::io("remove stale C archive", &path, source))?;
        }
        match self.flavor {
            CompilerFlavor::GnuLike => {
                self.assemble_gnu(spec, &path, &objects)?;
            }
            CompilerFlavor::Msvc => {
                let mut command = self.toolchain.archiver_command();
                command.arg(format!("/OUT:{}", path.display())).args(&objects);
                run(&mut command, "assemble C archive", &path)?;
            }
        }
        if !path.is_file() {
            return Err(Error::MissingArtifact {
                operation: "assemble C archive",
                path,
            });
        }
        if spec.cargo_metadata == Some(true) {
            println!("cargo:rustc-link-search=native={}", self.environment.output.display());
            println!("cargo:rustc-link-lib=static={}", spec.name);
        }
        Ok(Archive::new(path))
    }

    fn assemble_gnu(&self, spec: &ArchiveSpec, path: &std::path::Path, objects: &[PathBuf]) -> Result<()> {
        let mut command = self.gnu_archiver(spec);
        command.arg("crsD").arg(path).args(objects);
        let status = command
            .status()
            .map_err(|source| Error::io("assemble deterministic C archive", path, source))?;
        if status.success() {
            return Ok(());
        }
        let _ = std::fs::remove_file(path);
        let mut fallback = self.gnu_archiver(spec);
        fallback.env("ZERO_AR_DATE", "1").arg("crs").arg(path).args(objects);
        run(&mut fallback, "assemble C archive", path)
    }

    fn gnu_archiver(&self, spec: &ArchiveSpec) -> std::process::Command {
        let mut command = self.toolchain.archiver_command();
        if spec.archive_format == ArchiveFormat::Darwin && !self.environment.host.as_str().ends_with("-apple-darwin") {
            command.arg("--format=darwin");
        }
        command
    }
}

fn configure_compiler(command: &mut std::process::Command, flavor: CompilerFlavor, spec: &ArchiveSpec) {
    match flavor {
        CompilerFlavor::GnuLike => configure_gnu(command, spec),
        CompilerFlavor::Msvc => configure_msvc(command, spec),
    }
}

fn configure_gnu(command: &mut std::process::Command, spec: &ArchiveSpec) {
    if let Some(level) = spec.optimization {
        command.arg(format!("-O{level}"));
    }
    if spec.debug == Some(true) {
        command.arg("-g");
    }
    if spec.pic == Some(true) {
        command.arg("-fPIC");
    }
    if spec.language == Some(LanguageStandard::C11) {
        command.arg("-std=c11");
    }
    for include in &spec.includes {
        command.arg("-I").arg(include);
    }
    for definition in &spec.definitions {
        command.arg(define_argument("-D", definition));
    }
    if let Some(prelude) = &spec.forced_include {
        command.arg("-include").arg(prelude);
    }
    if let Some(visibility) = spec.visibility {
        command.arg(match visibility {
            Visibility::Default => "-fvisibility=default",
            Visibility::Hidden => "-fvisibility=hidden",
        });
    }
    if spec.function_sections == Some(false) {
        command.arg("-fno-function-sections");
    }
    if spec.data_sections == Some(false) {
        command.arg("-fno-data-sections");
    }
    if let Some(sanitizer) = spec.sanitizer {
        command.arg(match sanitizer {
            Sanitizer::Leak => "-fsanitize=leak",
            Sanitizer::Address => "-fsanitize=address",
        });
    }
    if spec.omit_frame_pointer == Some(false) {
        command.arg("-fno-omit-frame-pointer");
    }
    if spec.warnings_enabled != Some(false) {
        for warning in &spec.warnings {
            command.arg(warning.flag());
        }
    }
}

fn configure_msvc(command: &mut std::process::Command, spec: &ArchiveSpec) {
    if let Some(level) = spec.optimization {
        command.arg(if level == 0 { "/Od" } else { "/O2" });
    }
    if spec.debug == Some(true) {
        command.arg("/Zi");
    }
    if spec.language == Some(LanguageStandard::C11) {
        command.arg("/std:c11");
    }
    for include in &spec.includes {
        command.arg(format!("/I{}", include.display()));
    }
    for definition in &spec.definitions {
        command.arg(define_argument("/D", definition));
    }
    if let Some(prelude) = &spec.forced_include {
        command.arg(format!("/FI{}", prelude.display()));
    }
}

fn define_argument(prefix: &str, definition: &Definition) -> String {
    let mut argument = format!("{prefix}{}", definition.name());
    if let Some(value) = definition.replacement() {
        argument.push('=');
        argument.push_str(value);
    }
    argument
}

fn run(command: &mut std::process::Command, operation: &'static str, path: &std::path::Path) -> Result<()> {
    let status = command.status().map_err(|source| Error::io(operation, path, source))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::ToolFailed {
            operation,
            path: path.to_owned(),
            status,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ArchiveSpec, Definition, LanguageStandard, Visibility, Warning};
    #[test]
    fn compile_options_are_explicit_in_the_archive_spec() {
        let spec = ArchiveSpec::new("core")
            .sources(["a.c"])
            .language(LanguageStandard::C11)
            .optimization(2)
            .pic(true)
            .visibility(Visibility::Hidden)
            .warnings([Warning::All]);
        assert_eq!(spec.language, Some(LanguageStandard::C11));
        assert_eq!(spec.optimization, Some(2));
        assert_eq!(spec.pic, Some(true));
        assert_eq!(spec.visibility, Some(Visibility::Hidden));
        assert_eq!(spec.warnings, [Warning::All]);
    }

    #[test]
    fn definition_values_are_not_parsed_as_compound_strings() {
        let spec = ArchiveSpec::new("core").definitions([Definition::value("EXPRESSION", "left=right")]);
        let definition = &spec.definitions[0];
        assert_eq!(definition.name(), "EXPRESSION");
        assert_eq!(definition.replacement(), Some("left=right"));
    }

    #[test]
    #[should_panic(expected = "invalid C definition name")]
    fn definition_names_reject_command_line_syntax() {
        let _ = Definition::flag("NAME=value");
    }
}
