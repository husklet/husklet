use std::{fmt::Write, fs, process::Command};

use crate::{Archive, BuildEnvironment, Error, Result, Sanitizer, Toolchain};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkerFlavor {
    Elf,
    Darwin,
    GnuWindows,
    MsvcWindows,
}

#[must_use]
pub struct SharedLibrarySpec<'a> {
    pub name: &'a str,
    pub filename: &'a str,
    pub archives: Vec<Archive>,
    pub libraries: &'a [&'a str],
    pub exports: &'a [&'a str],
    pub excluded_symbols: &'a [&'a str],
    pub elf_version: Option<&'a str>,
    pub install_name: Option<&'a str>,
    pub soname: Option<&'a str>,
    pub symbolic_functions: bool,
    pub require_defined_symbols: bool,
    pub sanitizer: Option<Sanitizer>,
    pub whole_archive: bool,
    pub exclude_all_symbols: bool,
}

impl<'a> SharedLibrarySpec<'a> {
    pub const fn new(name: &'a str, filename: &'a str) -> Self {
        Self {
            name,
            filename,
            archives: Vec::new(),
            libraries: &[],
            exports: &[],
            excluded_symbols: &[],
            elf_version: None,
            install_name: None,
            soname: None,
            symbolic_functions: false,
            require_defined_symbols: false,
            sanitizer: None,
            whole_archive: false,
            exclude_all_symbols: false,
        }
    }
    pub fn archives(mut self, values: impl IntoIterator<Item = Archive>) -> Self {
        self.archives.extend(values);
        self
    }
    pub const fn libraries(mut self, values: &'a [&'a str]) -> Self {
        self.libraries = values;
        self
    }
    pub const fn exports(mut self, values: &'a [&'a str]) -> Self {
        self.exports = values;
        self
    }
    pub const fn excluded_symbols(mut self, values: &'a [&'a str]) -> Self {
        self.excluded_symbols = values;
        self
    }
    pub const fn elf_version(mut self, value: &'a str) -> Self {
        self.elf_version = Some(value);
        self
    }
    pub const fn install_name(mut self, value: &'a str) -> Self {
        self.install_name = Some(value);
        self
    }
    pub const fn soname(mut self, value: &'a str) -> Self {
        self.soname = Some(value);
        self
    }
    pub const fn symbolic_functions(mut self, value: bool) -> Self {
        self.symbolic_functions = value;
        self
    }
    pub const fn require_defined_symbols(mut self, value: bool) -> Self {
        self.require_defined_symbols = value;
        self
    }
    pub const fn sanitizer(mut self, value: Sanitizer) -> Self {
        self.sanitizer = Some(value);
        self
    }
    pub const fn whole_archive(mut self, value: bool) -> Self {
        self.whole_archive = value;
        self
    }
    pub const fn exclude_all_symbols(mut self, value: bool) -> Self {
        self.exclude_all_symbols = value;
        self
    }

    pub fn link(self, environment: &BuildEnvironment, toolchain: &Toolchain, flavor: LinkerFlavor) -> Result<()> {
        let destination = environment.output.join(self.filename);
        let mut command = toolchain.linker_command();
        match flavor {
            LinkerFlavor::Darwin => self.darwin(environment, &mut command)?,
            LinkerFlavor::GnuWindows => self.gnu_windows(environment, &mut command)?,
            LinkerFlavor::MsvcWindows => self.msvc_windows(environment, &mut command)?,
            LinkerFlavor::Elf => self.elf(environment, &mut command)?,
        }
        command.arg("-o").arg(&destination);
        for library in self.libraries {
            command.arg(format!("-l{library}"));
        }
        match self.sanitizer {
            Some(Sanitizer::Leak) => {
                command.arg("-fsanitize=leak");
            }
            Some(Sanitizer::Address) => {
                command.arg("-fsanitize=address");
            }
            None => {}
        }
        let status = command
            .status()
            .map_err(|source| Error::io("link shared library", &destination, source))?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::ToolFailed {
                operation: "link shared library",
                path: destination,
                status,
            })
        }
    }

    fn darwin(&self, environment: &BuildEnvironment, command: &mut Command) -> Result<()> {
        let exports = environment.output.join(format!("{}.exports", self.name));
        fs::write(&exports, darwin_exports(self.exports))
            .map_err(|source| Error::io("write Darwin export list", &exports, source))?;
        command.arg("-dynamiclib");
        if let Some(name) = self.install_name {
            command.arg(format!("-Wl,-install_name,{name}"));
        }
        command.arg(format!("-Wl,-exported_symbols_list,{}", exports.display()));
        for archive in &self.archives {
            if self.whole_archive {
                command.arg(format!("-Wl,-force_load,{}", archive.path().display()));
            } else {
                append_archive(command, archive);
            }
        }
        Ok(())
    }

    fn gnu_windows(&self, environment: &BuildEnvironment, command: &mut Command) -> Result<()> {
        let definition = self.windows_definition(environment)?;
        command
            .arg("-shared")
            .arg(format!(
                "-Wl,--out-implib,{}",
                environment.output.join(format!("lib{}.dll.a", self.name)).display()
            ))
            .arg(&definition);
        if self.exclude_all_symbols {
            command.arg("-Wl,--exclude-all-symbols");
        }
        for symbol in self.excluded_symbols {
            command.arg(format!("-Wl,--exclude-symbols={symbol}"));
        }
        if self.whole_archive {
            command.arg("-Wl,--whole-archive");
        }
        for archive in &self.archives {
            append_archive(command, archive);
        }
        if self.whole_archive {
            command.arg("-Wl,--no-whole-archive");
        }
        Ok(())
    }

    fn msvc_windows(&self, environment: &BuildEnvironment, command: &mut Command) -> Result<()> {
        let definition = self.windows_definition(environment)?;
        command
            .arg("-shared")
            .arg(format!(
                "-Wl,/IMPLIB:{}",
                environment.output.join(format!("{}.lib", self.name)).display()
            ))
            .arg(format!("-Wl,/DEF:{}", definition.display()));
        for archive in &self.archives {
            if self.whole_archive {
                command.arg(format!("-Wl,/WHOLEARCHIVE:{}", archive.path().display()));
            } else {
                append_archive(command, archive);
            }
        }
        Ok(())
    }

    fn windows_definition(&self, environment: &BuildEnvironment) -> Result<std::path::PathBuf> {
        let definition = environment.output.join(format!("{}.def", self.name));
        fs::write(&definition, windows_exports(self.exports))
            .map_err(|source| Error::io("write Windows export definition", &definition, source))?;
        Ok(definition)
    }

    fn elf(&self, environment: &BuildEnvironment, command: &mut Command) -> Result<()> {
        let version = self.elf_version.ok_or_else(|| Error::InvalidPlan {
            operation: "link ELF shared library",
            path: environment.output.join(self.filename),
            message: "an explicit version namespace is required".to_owned(),
        })?;
        let map = environment.output.join(format!("{}.map", self.name));
        fs::write(&map, elf_exports(version, self.exports))
            .map_err(|source| Error::io("write ELF export map", &map, source))?;
        command.arg("-shared");
        if let Some(soname) = self.soname {
            command.arg(format!("-Wl,-soname,{soname}"));
        }
        if self.symbolic_functions {
            command.arg("-Wl,-Bsymbolic-functions");
        }
        if self.require_defined_symbols {
            command.arg("-Wl,-z,defs");
        }
        if self.whole_archive {
            command.arg("-Wl,--whole-archive");
        }
        command.arg(format!("-Wl,--version-script={}", map.display()));
        for archive in &self.archives {
            append_archive(command, archive);
        }
        if self.whole_archive {
            command.arg("-Wl,--no-whole-archive");
        }
        Ok(())
    }
}

fn append_archive(command: &mut Command, archive: &Archive) {
    command.arg(archive.path());
}

fn darwin_exports(symbols: &[&str]) -> String {
    let mut output = String::new();
    for symbol in symbols {
        writeln!(output, "_{symbol}").expect("writing to a String cannot fail");
    }
    output
}
fn windows_exports(symbols: &[&str]) -> String {
    let mut output = "EXPORTS\n".to_owned();
    for symbol in symbols {
        writeln!(output, "  {symbol}").expect("writing to a String cannot fail");
    }
    output
}
fn elf_exports(version: &str, symbols: &[&str]) -> String {
    let mut output = format!("{version} {{\n  global:\n");
    for symbol in symbols {
        writeln!(output, "    {symbol};").expect("writing to a String cannot fail");
    }
    output.push_str("  local: *;\n};\n");
    output
}

#[cfg(test)]
mod tests {
    use super::{LinkerFlavor, SharedLibrarySpec, append_archive};
    use crate::Archive;
    use std::{path::PathBuf, process::Command};
    #[test]
    fn linker_flavor_and_platform_options_are_explicit() {
        let spec = SharedLibrarySpec::new("library", "library.so")
            .soname("library.so")
            .symbolic_functions(true);
        assert_eq!(LinkerFlavor::Elf, LinkerFlavor::Elf);
        assert_eq!(spec.soname, Some("library.so"));
        assert!(spec.symbolic_functions);
    }

    #[test]
    fn archive_artifact_paths_are_preserved_exactly() {
        let path = PathBuf::from("/unusual/archive-name.without-convention");
        let spec = SharedLibrarySpec::new("library", "library.so").archives([Archive::new(path.clone())]);
        assert_eq!(spec.archives[0].path(), path);
        let mut command = Command::new("linker");
        append_archive(&mut command, &spec.archives[0]);
        assert_eq!(command.get_args().collect::<Vec<_>>(), [path.as_os_str()]);
    }
}
