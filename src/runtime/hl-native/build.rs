use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
};

#[path = "src/artifact.rs"]
mod artifact;
#[path = "src/build_support.rs"]
mod build_support;
#[path = "src/platform.rs"]
mod platform;

const NATIVE_ROOT: &str = "src/native";
const COMMON_DEFINITIONS: &[&str] = &["HL_ENABLE_LOGGING=0", "HL_TRANSLIT_DEFAULT=0"];
const RUST_BRIDGE_EXPORTS: &[&str] = &[
    "hl_engine_abi",
    "hl_engine_version",
    "hl_c_backend_leak_check_nonvacuity",
    "hl_c_backend_checkpoint_broker_pair",
    "hl_c_backend_checkpoint_broker_accept",
    "hl_c_backend_checkpoint_trigger_create",
    "hl_c_backend_checkpoint_trigger_bump",
    "hl_c_backend_checkpoint_trigger_destroy",
    "hl_c_backend_checkpoint_adopt",
    "hl_c_backend_checkpoint_interrupt_signal",
    "hl_c_backend_create",
    "hl_c_backend_run",
    "hl_c_backend_request",
    "hl_c_backend_exit_kind",
    "hl_c_backend_exit_status",
    "hl_c_backend_exit_detail",
    "hl_c_backend_translation_count",
    "hl_c_backend_destroy",
    "hl_c_backend_executable_open",
    "hl_c_backend_executable_discard",
];
fn main() {
    for input in build_support::BUILD_POLICY_INPUTS {
        println!("cargo:rerun-if-changed={input}");
    }
    for input in build_support::COMPILER_ENVIRONMENT_INPUTS {
        println!("cargo:rerun-if-env-changed={input}");
    }
    for input in build_support::LINKER_ENVIRONMENT_INPUTS {
        println!("cargo:rerun-if-env-changed={input}");
    }
    let target_triple = env::var("TARGET").expect("Cargo supplies TARGET");
    for input in build_support::target_compiler_environment_inputs(&target_triple) {
        println!("cargo:rerun-if-env-changed={input}");
    }
    println!("cargo:rerun-if-env-changed=HL_C_SANITIZER");
    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo supplies CARGO_CFG_TARGET_OS");
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo supplies CARGO_CFG_TARGET_ARCH");
    let Some(target) = platform::HostTarget::from_cfg(&target_os, &target_arch) else {
        println!("cargo:supported=0");
        println!("cargo:warning=native C engine unavailable for {target_arch}-{target_os}");
        return;
    };
    if !target.supported() {
        println!("cargo:supported=0");
        println!(
            "cargo:rustc-env=HL_NATIVE_LIBRARY_NAME={}",
            artifact::filename(&target_os)
        );
        println!(
            "cargo:rustc-env=HL_NATIVE_LIBRARY_PATH={}",
            artifact::filename(&target_os)
        );
        println!("cargo:warning=native C engine planned but not yet verified for {target_arch}-{target_os}");
        return;
    }
    println!("cargo:supported=1");
    println!("cargo:host_os={target_os}");
    println!("cargo:host_arch={target_arch}");

    emit_native_rerun_inputs(Path::new(NATIVE_ROOT));
    let runtime_sources = discover_runtime_roots(&target_os, &target_arch);
    let runtime_source_refs = runtime_sources.iter().map(String::as_str).collect::<Vec<_>>();

    let platform_definition = if target_os == "macos" {
        "_DARWIN_C_SOURCE"
    } else {
        "_GNU_SOURCE"
    };
    compile(
        "hl_c_backend_shim",
        &[
            "src/native/bridge/shim.c",
            "src/native/bridge/host.c",
            "src/native/bridge/executable_authority.c",
            "src/native/bridge/address_projection.c",
        ],
        &[platform_definition],
        false,
    );
    compile("hl_c_backend_runtime", &runtime_source_refs, COMMON_DEFINITIONS, true);
    compile(
        "hl_c_backend_target_aarch64",
        &["src/native/engine/target/aarch64.c"],
        &[
            "HL_ENABLE_LOGGING=0",
            "HL_TRANSLIT_DEFAULT=0",
            "_GNU_SOURCE",
            "HL_EMBEDDED_BUILD=1",
            "HL_ENGINE_NO_MAIN=1",
            "HL_ENGINE_NO_STANDALONE=1",
            "HL_TARGET_NAMESPACE=aarch64",
        ],
        false,
    );
    compile(
        "hl_c_backend_target_x86_64",
        &["src/native/engine/target/x86_64.c"],
        &[
            "HL_ENABLE_LOGGING=0",
            "HL_TRANSLIT_DEFAULT=0",
            "_GNU_SOURCE",
            "HL_EMBEDDED_BUILD=1",
            "HL_ENGINE_NO_MAIN=1",
            "HL_ENGINE_NO_STANDALONE=1",
            "HL_TARGET_NAMESPACE=x86_64",
        ],
        false,
    );
    compile(
        "hl_c_backend_lifecycle_aarch64",
        &["src/native/engine/lifecycle.c"],
        &[
            "HL_ENABLE_LOGGING=0",
            "HL_TRANSLIT_DEFAULT=0",
            "_GNU_SOURCE",
            "HL_EMBEDDED_BUILD=1",
            "HL_TARGET_NAMESPACE=aarch64",
            "HL_PRODUCTION_GUEST_ISA=HL_GUEST_ISA_AARCH64",
        ],
        false,
    );
    compile(
        "hl_c_backend_lifecycle_x86_64",
        &["src/native/engine/lifecycle.c"],
        &[
            "HL_ENABLE_LOGGING=0",
            "HL_TRANSLIT_DEFAULT=0",
            "_GNU_SOURCE",
            "HL_EMBEDDED_BUILD=1",
            "HL_TARGET_NAMESPACE=x86_64",
            "HL_PRODUCTION_GUEST_ISA=HL_GUEST_ISA_X86_64",
        ],
        false,
    );

    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"));
    let archives = [
        "hl_c_backend_shim",
        "hl_c_backend_target_aarch64",
        "hl_c_backend_target_x86_64",
        "hl_c_backend_lifecycle_aarch64",
        "hl_c_backend_lifecycle_x86_64",
        "hl_c_backend_runtime",
    ];
    let system_libraries = match target_os.as_str() {
        "macos" => &["m", "pthread"][..],
        "windows" => build_support::WINDOWS_SYSTEM_LIBRARIES,
        _ => &["atomic", "dl", "m", "pthread"][..],
    };
    link_shared_engine(&output, &target_os, &archives, system_libraries);
    println!("cargo:rustc-link-search=native={}", output.display());
    println!("cargo:rustc-link-lib=dylib=hl_native_engine");
    emit_loader_paths(
        &output,
        &target_os,
        &env::var("PROFILE").expect("Cargo supplies PROFILE"),
    );
    if env::var("HL_C_SANITIZER").as_deref() == Ok("leak") && target_os == "linux" {
        println!("cargo:rustc-link-lib=lsan");
    }
}

fn emit_native_rerun_inputs(directory: &Path) {
    println!("cargo:rerun-if-changed={}", directory.display());
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("enumerate {}: {error}", directory.display()));
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            emit_native_rerun_inputs(&path);
        } else if path.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

fn discover_runtime_roots(target_os: &str, target_arch: &str) -> Vec<String> {
    let root = Path::new(NATIVE_ROOT);
    let mut sources = BTreeSet::new();
    collect_c_sources(root, &mut sources);
    let included = sources
        .iter()
        .flat_map(|source| included_c_sources(source))
        .collect::<BTreeSet<_>>();
    let special = [
        "src/native/bridge/shim.c",
        "src/native/bridge/host.c",
        "src/native/bridge/executable_authority.c",
        "src/native/bridge/address_projection.c",
        "src/native/engine/lifecycle.c",
        "src/native/engine/target/aarch64.c",
        "src/native/engine/target/x86_64.c",
    ];
    sources
        .into_iter()
        .filter(|source| !included.contains(source))
        .filter(|source| !special.contains(&source.to_string_lossy().as_ref()))
        .filter(|source| build_support::source_matches(target_os, &source.to_string_lossy()))
        .filter(|source| {
            target_arch == "aarch64" || !source.to_string_lossy().contains("/translator/guest/x86_64/lower/")
        })
        .map(|source| source.to_string_lossy().into_owned())
        .collect()
}

fn collect_c_sources(directory: &Path, output: &mut BTreeSet<PathBuf>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("enumerate {}: {error}", directory.display()));
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_c_sources(&path, output);
        } else if path.extension().and_then(|value| value.to_str()) == Some("c") {
            output.insert(path);
        }
    }
}

fn included_c_sources(source: &Path) -> Vec<PathBuf> {
    let text = fs::read_to_string(source).unwrap_or_else(|error| panic!("read {}: {error}", source.display()));
    text.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let include = line.strip_prefix("#include \"")?.split('"').next()?;
            Path::new(include)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("c"))
                .then(|| {
                    let relative = source.parent().expect("source parent").join(include);
                    relative.canonicalize().unwrap_or_else(|error| {
                        panic!("resolve C include {include} from {}: {error}", source.display())
                    })
                })
        })
        .map(|path| {
            let package = env::current_dir()
                .expect("current package")
                .canonicalize()
                .expect("canonical package");
            path.strip_prefix(package).expect("package-local C include").to_owned()
        })
        .collect()
}

fn compile(name: &str, sources: &[&str], definitions: &[&str], strict: bool) {
    let mut build = cc::Build::new();
    let debug = env::var("PROFILE").as_deref() != Ok("release");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        build.archiver("/usr/bin/ar");
    }
    build
        .cargo_metadata(false)
        .include(NATIVE_ROOT)
        .include("src/native/include")
        .include("src/native")
        .opt_level(2)
        .debug(debug)
        .pic(true)
        .warnings(strict)
        .std("c11")
        .define("HL_SHARED", None)
        .define("HL_BUILDING_ENGINE", None)
        .flag_if_supported("-fvisibility=hidden")
        .flag_if_supported("-fno-function-sections")
        .flag_if_supported("-fno-data-sections");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // LLVM's archiver defaults to the GNU archive dialect even on Darwin.
        // Apple ld can inspect its members but will not force-load it reliably.
        // The development shell's AR names a Linux guest cross-archiver, so do
        // not let that guest-build setting escape into this host artifact.
        build.archiver("/usr/bin/ar");
        build.ar_flag("--format=darwin");
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        build.include("src/native/toolchain/msvc-posix/include");
        let prelude = "src/native/toolchain/msvc-posix/include/prelude.h";
        if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            build.flag(format!("/FI{prelude}"));
        } else {
            build.flag("-include").flag(prelude);
        }
    }
    if name == "hl_c_backend_shim" {
        // This archive is the narrow Rust/C bridge. The engine itself stays
        // hidden; only bridge entry points and the versioned public C ABI are
        // visible from the shared object.
        build.flag_if_supported("-fvisibility=default");
    }
    match env::var("HL_C_SANITIZER").as_deref() {
        Ok("leak") => {
            build
                .opt_level(1)
                .flag("-fsanitize=leak")
                .flag("-fno-omit-frame-pointer")
                .define("HL_LEAK_CHECK_PROBE", None)
                .define("HL_LEAK_SANITIZER", None);
        }
        Ok("memcheck") => {
            // Valgrind supplies the instrumentation itself. This definition
            // only retains the deliberately leaking non-vacuity hook in the
            // otherwise ordinary native build used by that independent gate.
            build.define("HL_LEAK_CHECK_PROBE", None);
        }
        Ok(value) => panic!("unsupported HL_C_SANITIZER={value:?}; expected leak or memcheck"),
        Err(env::VarError::NotPresent) => {}
        Err(error) => panic!("invalid HL_C_SANITIZER: {error}"),
    }
    if strict {
        for flag in [
            "-Wall",
            "-Wextra",
            "-Wpedantic",
            "-Wconversion",
            "-Wshadow",
            "-Wstrict-prototypes",
            "-Wmissing-prototypes",
            "-Werror=implicit-function-declaration",
            "-Werror=implicit-int",
        ] {
            build.flag_if_supported(flag);
        }
    }
    for definition in definitions {
        if let Some((name, value)) = definition.split_once('=') {
            build.define(name, value);
        } else {
            build.define(definition, None);
        }
    }
    for source in sources {
        build.file(source);
    }
    build.compile(name);
}

fn link_shared_engine(output: &Path, target_os: &str, archives: &[&str], libraries: &[&str]) {
    let compiler = cc::Build::new().get_compiler();
    let filename = artifact::filename(target_os);
    let destination = output.join(filename);
    let mut command = compiler.to_command();
    if target_os == "macos" {
        let export_list = output.join("hl_native_engine.exports");
        let exported = build_support::darwin_export_list(RUST_BRIDGE_EXPORTS);
        fs::write(&export_list, exported).expect("write Darwin native export list");
        command.args(["-dynamiclib", "-Wl,-install_name,@rpath/libhl_native_engine.dylib"]);
        command.arg(format!("-Wl,-exported_symbols_list,{}", export_list.display()));
        for archive in archives {
            command.arg(format!(
                "-Wl,-force_load,{}",
                output.join(format!("lib{archive}.a")).display()
            ));
        }
    } else if target_os == "windows" {
        let definition = output.join("hl_native_engine.def");
        let exported = build_support::windows_export_definition(RUST_BRIDGE_EXPORTS);
        fs::write(&definition, exported).expect("write Windows native export definition");
        command.arg("-shared");
        command.arg(format!("-Wl,/IMPLIB:{}", output.join("hl_native_engine.lib").display()));
        command.arg(format!("-Wl,/DEF:{}", definition.display()));
        for archive in archives {
            command.arg(format!(
                "-Wl,/WHOLEARCHIVE:{}",
                output
                    .join(build_support::static_archive_filename(target_os, archive))
                    .display()
            ));
        }
    } else {
        let export_map = output.join("hl_native_engine.map");
        fs::write(&export_map, build_support::linux_export_map(RUST_BRIDGE_EXPORTS))
            .expect("write Linux native export map");
        command.args([
            "-shared",
            "-Wl,-soname,libhl_native_engine.so",
            "-Wl,-Bsymbolic-functions",
            "-Wl,-z,defs",
            "-Wl,--whole-archive",
        ]);
        command.arg(format!("-Wl,--version-script={}", export_map.display()));
        for archive in archives {
            command.arg(output.join(format!("lib{archive}.a")));
        }
        command.arg("-Wl,--no-whole-archive");
    }
    command.arg("-o").arg(&destination);
    for library in libraries {
        command.arg(format!("-l{library}"));
    }
    if env::var("HL_C_SANITIZER").as_deref() == Ok("leak") && target_os == "linux" {
        command.arg("-fsanitize=leak");
    }
    let status = command
        .status()
        .unwrap_or_else(|error| panic!("link {}: {error}", destination.display()));
    assert!(status.success(), "native shared-library link failed with {status}");
    println!("cargo:rustc-env=HL_NATIVE_LIBRARY_NAME={filename}");
    println!("cargo:rustc-env=HL_NATIVE_LIBRARY_PATH={}", destination.display());
}

fn emit_loader_paths(output: &Path, target_os: &str, profile: &str) {
    // Keep relocatable package locations before the development fallback.
    // Windows has no rpath; its package places the DLL beside each executable.
    for path in artifact::loader_paths(target_os) {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{path}");
    }
    // Development and test executables run directly from Cargo's target tree.
    // Installed release products must not retain a workspace-specific OUT_DIR.
    if target_os != "windows" && profile != "release" {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", output.display());
    }
}
