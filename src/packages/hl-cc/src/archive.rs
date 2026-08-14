use std::path::PathBuf;

use crate::BuildEnvironment;

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
    pub definitions: Vec<String>,
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
    pub fn definitions(mut self, values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.definitions.extend(values.into_iter().map(Into::into));
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
    flavor: CompilerFlavor,
}

impl<'a> CCompiler<'a> {
    pub const fn new(environment: &'a BuildEnvironment, flavor: CompilerFlavor) -> Self {
        Self { environment, flavor }
    }
    pub fn archive(&self, spec: &ArchiveSpec) {
        let mut build = cc::Build::new();
        if let Some(value) = spec.cargo_metadata {
            build.cargo_metadata(value);
        }
        if let Some(level) = spec.optimization {
            build.opt_level(level);
        }
        if let Some(debug) = spec.debug {
            build.debug(debug);
        }
        if let Some(pic) = spec.pic {
            build.pic(pic);
        }
        if let Some(enabled) = spec.warnings_enabled {
            build.warnings(enabled);
        }
        if let Some(LanguageStandard::C11) = spec.language {
            build.std("c11");
        }
        for include in &spec.includes {
            build.include(include);
        }
        if spec.archive_format == ArchiveFormat::Darwin {
            build.archiver("/usr/bin/ar");
            if !self.environment.host.as_str().ends_with("-apple-darwin") {
                build.ar_flag("--format=darwin");
            }
        }
        if let Some(prelude) = &spec.forced_include {
            match self.flavor {
                CompilerFlavor::Msvc => {
                    build.flag(format!("/FI{}", prelude.display()));
                }
                CompilerFlavor::GnuLike => {
                    build.flag("-include").flag(prelude);
                }
            }
        }
        if let Some(visibility) = spec.visibility {
            build.flag_if_supported(match visibility {
                Visibility::Default => "-fvisibility=default",
                Visibility::Hidden => "-fvisibility=hidden",
            });
        }
        if spec.function_sections == Some(false) {
            build.flag_if_supported("-fno-function-sections");
        }
        if spec.data_sections == Some(false) {
            build.flag_if_supported("-fno-data-sections");
        }
        match spec.sanitizer {
            Some(Sanitizer::Leak) => {
                build.flag("-fsanitize=leak");
            }
            Some(Sanitizer::Address) => {
                build.flag("-fsanitize=address");
            }
            None => {}
        }
        if spec.omit_frame_pointer == Some(false) {
            build.flag("-fno-omit-frame-pointer");
        }
        for warning in &spec.warnings {
            build.flag_if_supported(warning.flag());
        }
        for definition in &spec.definitions {
            if let Some((name, value)) = definition.split_once('=') {
                build.define(name, value);
            } else {
                build.define(definition, None);
            }
        }
        for source in &spec.sources {
            build.file(source);
        }
        build.compile(&spec.name);
    }
}

#[cfg(test)]
mod tests {
    use super::{ArchiveSpec, LanguageStandard, Visibility, Warning};
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
}
