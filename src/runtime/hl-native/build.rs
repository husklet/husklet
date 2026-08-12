use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
};

#[path = "src/platform.rs"]
mod platform;

const NATIVE_ROOT: &str = "src/native";
const COMMON_DEFINITIONS: &[&str] = &["HL_ENABLE_LOGGING=0", "HL_TRANSLIT_DEFAULT=0"];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=HL_C_SANITIZER");
    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo supplies CARGO_CFG_TARGET_OS");
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo supplies CARGO_CFG_TARGET_ARCH");
    let Some(target) = platform::HostTarget::from_cfg(&target_os, &target_arch) else {
        println!("cargo:supported=0");
        println!("cargo:warning=native C engine unavailable for {target_arch}-{target_os}");
        return;
    };
    println!("cargo:supported=1");
    println!("cargo:host_os={}", target.os.cfg_name());
    println!("cargo:host_arch={}", target.arch.cfg_name());

    println!("cargo:rerun-if-changed={NATIVE_ROOT}");
    let runtime_sources = discover_runtime_roots(&target_os);
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
        "windows" => platform::WINDOWS_SYSTEM_LIBRARIES,
        _ => &["atomic", "dl", "m", "pthread"][..],
    };
    link_shared_engine(&output, &target_os, &archives, system_libraries);
    println!("cargo:rustc-link-search=native={}", output.display());
    println!("cargo:rustc-link-lib=dylib=hl_native_engine");
    emit_development_rpath(&output, &target_os);
    if env::var("HL_C_SANITIZER").as_deref() == Ok("leak") && target_os == "linux" {
        println!("cargo:rustc-link-lib=lsan");
    }
}

fn discover_runtime_roots(target_os: &str) -> Vec<String> {
    let root = Path::new(NATIVE_ROOT);
    let mut sources = BTreeSet::new();
    collect_c_sources(root, &mut sources);
    let included = sources
        .iter()
        .flat_map(|source| included_c_sources(source))
        .collect::<BTreeSet<_>>();
    let special = [
        "src/native/bridge/shim.c",
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
        .filter(|source| platform::source_matches(target_os, &source.to_string_lossy()))
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
    build
        .cargo_metadata(false)
        .include(NATIVE_ROOT)
        .include("src/native/include")
        .include("src/native")
        .opt_level(2)
        .debug(true)
        .pic(true)
        .warnings(strict)
        .std("c11")
        .define("HL_SHARED", None)
        .define("HL_BUILDING_ENGINE", None)
        .flag_if_supported("-fvisibility=hidden")
        .flag_if_supported("-fno-function-sections")
        .flag_if_supported("-fno-data-sections");
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
                .define("HL_LEAK_CHECK_PROBE", None);
        }
        Ok(value) => panic!("unsupported HL_C_SANITIZER={value:?}; expected leak"),
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
    let filename = shared_library_filename(target_os);
    let destination = output.join(filename);
    let mut command = compiler.to_command();
    if target_os == "macos" {
        command.args(["-dynamiclib", "-Wl,-install_name,@rpath/libhl_native_engine.dylib"]);
        for archive in archives {
            command.arg(format!(
                "-Wl,-force_load,{}",
                output.join(format!("lib{archive}.a")).display()
            ));
        }
    } else if target_os == "windows" {
        command.arg("-shared");
        command.arg(format!("-Wl,/IMPLIB:{}", output.join("hl_native_engine.lib").display()));
        for archive in archives {
            command.arg(format!(
                "-Wl,/WHOLEARCHIVE:{}",
                output.join(platform::static_archive_filename(target_os, archive)).display()
            ));
        }
    } else {
        command.args([
            "-shared",
            "-Wl,-soname,libhl_native_engine.so",
            "-Wl,-Bsymbolic-functions",
            "-Wl,-z,defs",
            "-Wl,--whole-archive",
        ]);
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
}

fn shared_library_filename(target_os: &str) -> &'static str {
    match target_os {
        "macos" => "libhl_native_engine.dylib",
        "windows" => "hl_native_engine.dll",
        _ => "libhl_native_engine.so",
    }
}

fn emit_development_rpath(output: &Path, target_os: &str) {
    if target_os != "windows" {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", output.display());
    }
}
