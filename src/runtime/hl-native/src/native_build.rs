use std::path::Path;

use hl_cc::{
    ArchiveFormat, ArchiveSpec, BuildEnvironment, CCompiler, CargoDirectives, Definition, EnvFlag, EnvKey,
    LanguageStandard, LinkerFlavor, Sanitizer, SharedLibrarySpec, TargetTools, Toolchain, Visibility, Warning,
};

mod artifact;
mod build_support;
mod inventory;
mod platform;

use platform::{GuestIsa, HostTarget};

const NATIVE_ROOT: &str = "src/native";
const NATIVE_FINGERPRINT: &str = "HL_NATIVE_BUILD_FINGERPRINT";
const NATIVE_TEST_HOOKS: EnvFlag = EnvFlag::new("CARGO_FEATURE_NATIVE_TEST_HOOKS");
const NATIVE_COMPILE_CHECK: EnvFlag = EnvFlag::new("HL_NATIVE_COMPILE_CHECK");
const C_SANITIZER: EnvKey<NativeSanitizer> = EnvKey::new("HL_C_SANITIZER", NativeSanitizer::parse);
const RUST_BRIDGE_EXPORTS: &str = include_str!("native/bridge/exports.txt");
const TEST_HOOK_EXPORTS: &str = include_str!("native/bridge/test_exports.txt");
const DARWIN_LIBRARIES: &[&str] = &["bsm", "m", "pthread"];
const ELF_LIBRARIES: &[&str] = &["atomic", "dl", "m", "pthread"];

fn main() {
    let environment = BuildEnvironment::from_cargo().unwrap_or_else(|error| panic!("{error}"));
    emit_build_inputs(environment.target.as_str());
    let test_hooks = environment.flag(NATIVE_TEST_HOOKS);
    let compile_check = environment.flag(NATIVE_COMPILE_CHECK);
    let sanitizer = environment
        .value(&C_SANITIZER)
        .unwrap_or_else(|error| panic!("{error}"))
        .unwrap_or(NativeSanitizer::None);
    let Some(target) = HostTarget::from_cfg(environment.target_os.as_str(), environment.target_arch.as_str()) else {
        CargoDirectives::cfg("supported", 0);
        CargoDirectives::warning(format!(
            "native C engine unavailable for {}-{}",
            environment.target_arch.as_str(),
            environment.target_os.as_str()
        ));
        return;
    };
    if !target.supported() && !compile_check {
        emit_planned_target(&environment);
        return;
    }
    CargoDirectives::cfg("supported", u8::from(target.supported()));
    CargoDirectives::cfg("host_os", environment.target_os.as_str());
    CargoDirectives::cfg("host_arch", environment.target_arch.as_str());
    inventory::sources::emit_rerun_inputs(Path::new(NATIVE_ROOT));
    // Every native source contributes to this value. It is compiled into the artifact and
    // exported as `cargo:rustc-env`, so a C edit invalidates the crate's own Cargo fingerprint
    // and the loader can refuse a shared object built from different sources than the caller.
    let fingerprint = inventory::fingerprint::native_fingerprint(Path::new(NATIVE_ROOT));
    CargoDirectives::rustc_environment(NATIVE_FINGERPRINT, &fingerprint);
    let plan = target.build_plan();

    let tools = TargetTools::resolve(&environment.target_os, &environment.target_environment)
        .unwrap_or_else(|| panic!("no C tool plan for {}", environment.target.as_str()));
    let toolchain = Toolchain::discover(&environment).unwrap_or_else(|error| panic!("{error}"));
    let compiler = CCompiler::new(&environment, &toolchain, tools.compiler);
    let mut shim_definitions = vec![Definition::value(NATIVE_FINGERPRINT, &fingerprint)];
    add_test_hooks(&mut shim_definitions, test_hooks);
    if plan.guests == [GuestIsa::X86_64] {
        shim_definitions.push(Definition::value("HL_BUILD_TARGET_X86_64_ONLY", "1"));
    }
    let shim_archive = compiler
        .archive(
            &archive(&environment, target, sanitizer, "hl_c_backend_shim", false)
                .sources([
                    "src/native/bridge/shim.c",
                    "src/native/bridge/table.c",
                    "src/native/bridge/host.c",
                    "src/native/bridge/executable_authority.c",
                    "src/native/bridge/address_projection.c",
                ])
                .definitions(shim_definitions)
                .visibility(Visibility::Default),
        )
        .unwrap_or_else(|error| panic!("{error}"));

    let runtime_sources = inventory::sources::runtime_roots(
        environment.target_os.as_str(),
        environment.target_arch.as_str(),
        &environment.manifest_directory,
    );
    let mut runtime_definitions = common_definitions();
    add_test_hooks(&mut runtime_definitions, test_hooks);
    let runtime_archive = compiler
        .archive(
            &archive(&environment, target, sanitizer, "hl_c_backend_runtime", true)
                .sources(runtime_sources)
                .definitions(runtime_definitions),
        )
        .unwrap_or_else(|error| panic!("{error}"));

    let mut linked_archives = vec![shim_archive];
    for guest in plan.guests {
        let (target_archive, target_source, lifecycle_archive) = match guest {
            GuestIsa::Aarch64 => (
                "hl_c_backend_target_aarch64",
                "src/native/engine/target/aarch64.c",
                "hl_c_backend_lifecycle_aarch64",
            ),
            GuestIsa::X86_64 => (
                "hl_c_backend_target_x86_64",
                "src/native/engine/target/x86_64.c",
                "hl_c_backend_lifecycle_x86_64",
            ),
        };
        let mut target_definitions = vec![
            Definition::value("HL_ENABLE_LOGGING", "0"),
            Definition::value("HL_TRANSLIT_DEFAULT", "0"),
            Definition::value("HL_EMBEDDED_BUILD", "1"),
        ];
        target_definitions.push(Definition::value(
            "HL_TARGET_NAMESPACE",
            match guest {
                GuestIsa::Aarch64 => "aarch64",
                GuestIsa::X86_64 => "x86_64",
            },
        ));
        if plan.guests == [GuestIsa::X86_64] && *guest == GuestIsa::X86_64 {
            target_definitions.push(Definition::value("HL_CKPT_INTERRUPT_EXPORT", "1"));
        }
        add_test_hooks(&mut target_definitions, test_hooks);
        let target_archive = compiler
            .archive(
                &archive(&environment, target, sanitizer, target_archive, false)
                    .sources([target_source])
                    .definitions(target_definitions),
            )
            .unwrap_or_else(|error| panic!("{error}"));

        let mut lifecycle_definitions = vec![
            Definition::value("HL_ENABLE_LOGGING", "0"),
            Definition::value("HL_TRANSLIT_DEFAULT", "0"),
            Definition::value("HL_EMBEDDED_BUILD", "1"),
            Definition::value(
                "HL_TARGET_NAMESPACE",
                match guest {
                    GuestIsa::Aarch64 => "aarch64",
                    GuestIsa::X86_64 => "x86_64",
                },
            ),
            Definition::value(
                "HL_PRODUCTION_GUEST_ISA",
                match guest {
                    GuestIsa::Aarch64 => "HL_GUEST_ISA_AARCH64",
                    GuestIsa::X86_64 => "HL_GUEST_ISA_X86_64",
                },
            ),
        ];
        add_test_hooks(&mut lifecycle_definitions, test_hooks);
        let lifecycle_archive = compiler
            .archive(
                &archive(&environment, target, sanitizer, lifecycle_archive, false)
                    .sources(["src/native/engine/lifecycle.c"])
                    .definitions(lifecycle_definitions),
            )
            .unwrap_or_else(|error| panic!("{error}"));
        linked_archives.extend([target_archive, lifecycle_archive]);
    }
    linked_archives.push(runtime_archive);

    let export_manifest = if test_hooks {
        TEST_HOOK_EXPORTS
    } else {
        RUST_BRIDGE_EXPORTS
    };
    let bridge_exports = build_support::export_symbols(export_manifest);
    let mut libraries = match target.os {
        platform::HostOs::Macos => DARWIN_LIBRARIES.to_vec(),
        platform::HostOs::Windows => build_support::WINDOWS_SYSTEM_LIBRARIES.to_vec(),
        platform::HostOs::Linux => ELF_LIBRARIES.to_vec(),
    };
    let linker = tools.linker;
    if linker == LinkerFlavor::GnuWindows {
        libraries.push("atomic");
    }
    let filename = artifact::filename(environment.target_os.as_str());
    let mut library = SharedLibrarySpec::new("hl_native_engine", filename)
        .archives(linked_archives)
        .libraries(&libraries)
        .exports(&bridge_exports)
        .excluded_symbols(&[
            "hl_aarch64_target_syscall_trap_install",
            "hl_x86_64_target_syscall_trap_install",
        ])
        .whole_archive(true)
        .exclude_all_symbols(linker == LinkerFlavor::GnuWindows)
        .elf_version("HL_NATIVE_1");
    library = match linker {
        LinkerFlavor::Darwin => library.install_name("@rpath/libhl_native_engine.dylib"),
        LinkerFlavor::Elf => library
            .soname("libhl_native_engine.so")
            .symbolic_functions(true)
            .require_defined_symbols(true),
        LinkerFlavor::GnuWindows | LinkerFlavor::MsvcWindows => library,
    };
    if target.os == platform::HostOs::Linux
        && let Some(value) = sanitizer.compiler()
    {
        library = library.sanitizer(value);
    }
    library
        .link(&environment, &toolchain, linker)
        .unwrap_or_else(|error| panic!("{error}"));
    CargoDirectives::rustc_environment("HL_NATIVE_LIBRARY_NAME", filename);
    CargoDirectives::rustc_environment("HL_NATIVE_LIBRARY_PATH", environment.output.join(filename).display());
    if target.os == platform::HostOs::Linux {
        CargoDirectives::link_library(None, "dl");
    }
    if sanitizer == NativeSanitizer::Leak && target.os == platform::HostOs::Linux {
        CargoDirectives::link_library(None, "lsan");
    }
}

fn archive(
    environment: &BuildEnvironment,
    target: HostTarget,
    sanitizer: NativeSanitizer,
    name: &'static str,
    strict: bool,
) -> ArchiveSpec {
    const STRICT_WARNINGS: &[Warning] = &[
        Warning::All,
        Warning::Extra,
        Warning::Pedantic,
        Warning::Conversion,
        Warning::Shadow,
        Warning::StrictPrototypes,
        Warning::MissingPrototypes,
        Warning::ImplicitFunctionDeclarationError,
        Warning::ImplicitIntError,
    ];
    let mut spec = ArchiveSpec::new(name)
        .includes([NATIVE_ROOT, "src/native/include", "src/native"])
        .definitions(engine_definitions())
        .definitions([Definition::flag(target.os.feature_definition())])
        .language(LanguageStandard::C11)
        .optimization(if sanitizer.compiler().is_some() { 1 } else { 2 })
        .debug(environment.profile != hl_cc::Profile::Release)
        .pic(true)
        .visibility(Visibility::Hidden)
        .function_sections(false)
        .data_sections(false)
        .warnings_enabled(strict)
        .cargo_metadata(false)
        .warnings(if strict { STRICT_WARNINGS.to_vec() } else { Vec::new() });
    if sanitizer.compiler().is_some() {
        spec = spec.omit_frame_pointer(false);
    }
    if target.os == platform::HostOs::Macos {
        spec = spec.archive_format(ArchiveFormat::Darwin);
    }
    if target.os == platform::HostOs::Windows {
        spec = spec
            .includes(["src/native/toolchain/msvc-posix/include"])
            .forced_include("src/native/toolchain/msvc-posix/include/prelude.h");
    }
    match sanitizer {
        NativeSanitizer::Leak => spec
            .definitions([
                Definition::flag("HL_LEAK_CHECK_PROBE"),
                Definition::flag("HL_LEAK_SANITIZER"),
            ])
            .sanitizer(Sanitizer::Leak),
        NativeSanitizer::Memcheck => spec.definitions([Definition::flag("HL_LEAK_CHECK_PROBE")]),
        NativeSanitizer::Address => spec
            .definitions([Definition::flag("HL_ADDRESS_SANITIZER")])
            .sanitizer(Sanitizer::Address),
        NativeSanitizer::None => spec,
    }
}

fn engine_definitions() -> [Definition; 3] {
    [
        Definition::flag("HL_SHARED"),
        Definition::flag("HL_BUILDING_ENGINE"),
        Definition::flag("HL_EXPLICIT_EXPORTS"),
    ]
}

fn common_definitions() -> Vec<Definition> {
    vec![
        Definition::value("HL_ENABLE_LOGGING", "0"),
        Definition::value("HL_TRANSLIT_DEFAULT", "0"),
    ]
}

fn add_test_hooks(definitions: &mut Vec<Definition>, enabled: bool) {
    if enabled {
        definitions.push(Definition::value("HL_NATIVE_TEST_HOOKS", "1"));
    }
}

fn emit_build_inputs(target_triple: &str) {
    for input in build_support::BUILD_POLICY_INPUTS {
        CargoDirectives::rerun_file(input);
    }
    for input in build_support::COMPILER_ENVIRONMENT_INPUTS {
        CargoDirectives::rerun_environment(input);
    }
    for input in build_support::LINKER_ENVIRONMENT_INPUTS {
        CargoDirectives::rerun_environment(input);
    }
    for input in build_support::target_compiler_environment_inputs(target_triple) {
        CargoDirectives::rerun_environment(&input);
    }
    CargoDirectives::rerun_environment("HL_C_SANITIZER");
    CargoDirectives::rerun_environment("HL_NATIVE_COMPILE_CHECK");
}

fn emit_planned_target(environment: &BuildEnvironment) {
    let filename = artifact::filename(environment.target_os.as_str());
    CargoDirectives::cfg("supported", 0);
    CargoDirectives::rustc_environment(NATIVE_FINGERPRINT, "unbuilt");
    CargoDirectives::rustc_environment("HL_NATIVE_LIBRARY_NAME", filename);
    CargoDirectives::rustc_environment("HL_NATIVE_LIBRARY_PATH", filename);
    CargoDirectives::warning(format!(
        "native C engine planned but not yet verified for {}-{}",
        environment.target_arch.as_str(),
        environment.target_os.as_str()
    ));
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeSanitizer {
    None,
    Address,
    Leak,
    Memcheck,
}

impl NativeSanitizer {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "address" => Ok(Self::Address),
            "leak" => Ok(Self::Leak),
            "memcheck" => Ok(Self::Memcheck),
            value => Err(format!(
                "unsupported value {value:?}; expected address, leak, or memcheck"
            )),
        }
    }
    const fn compiler(self) -> Option<Sanitizer> {
        match self {
            Self::Address => Some(Sanitizer::Address),
            Self::Leak => Some(Sanitizer::Leak),
            Self::None | Self::Memcheck => None,
        }
    }
}
