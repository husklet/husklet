use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
};

#[path = "src/platform.rs"]
mod platform;

const NATIVE_ROOT: &str = ".";
const COMMON_DEFINITIONS: &[&str] = &["HL_ENABLE_LOGGING=0", "HL_TRANSLIT_DEFAULT=0"];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=HL_C_SANITIZER");
    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo supplies CARGO_CFG_TARGET_OS");
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo supplies CARGO_CFG_TARGET_ARCH");
    if !platform::supported(&target_os, &target_arch) {
        println!("cargo:warning=native C engine unavailable for {target_arch}-{target_os}");
        return;
    }

    println!("cargo:rerun-if-changed=src");
    let runtime_sources = discover_runtime_roots(&target_os);
    let runtime_source_refs = runtime_sources.iter().map(String::as_str).collect::<Vec<_>>();

    let platform_definition = if target_os == "macos" {
        "_DARWIN_C_SOURCE"
    } else {
        "_GNU_SOURCE"
    };
    compile(
        "hl_c_backend_shim",
        &["src/shim.c", "src/executable_authority.c", "src/address_projection.c"],
        &[platform_definition],
        false,
    );
    compile("hl_c_backend_runtime", &runtime_source_refs, COMMON_DEFINITIONS, true);
    compile(
        "hl_c_backend_target_aarch64",
        &["src/core/target/aarch64.c"],
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
        &["src/core/target/x86_64.c"],
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
        &["src/core/lifecycle.c"],
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
        &["src/core/lifecycle.c"],
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
    println!("cargo:rustc-link-search=native={}", output.display());
    for archive in [
        "hl_c_backend_shim",
        "hl_c_backend_target_aarch64",
        "hl_c_backend_target_x86_64",
        "hl_c_backend_lifecycle_aarch64",
        "hl_c_backend_lifecycle_x86_64",
        "hl_c_backend_runtime",
    ] {
        println!("cargo:rustc-link-lib=static:+whole-archive={archive}");
    }
    for library in if target_os == "macos" {
        &["m", "pthread"][..]
    } else {
        &["atomic", "dl", "m", "pthread"][..]
    } {
        println!("cargo:rustc-link-lib={library}");
    }
    if env::var("HL_C_SANITIZER").as_deref() == Ok("leak") && target_os == "linux" {
        println!("cargo:rustc-link-lib=lsan");
    }
}

fn discover_runtime_roots(target_os: &str) -> Vec<String> {
    let root = Path::new("src");
    let mut sources = BTreeSet::new();
    collect_c_sources(root, root, &mut sources);
    let included = sources
        .iter()
        .flat_map(|source| included_c_sources(source))
        .collect::<BTreeSet<_>>();
    let special = [
        "src/shim.c",
        "src/executable_authority.c",
        "src/address_projection.c",
        "src/core/lifecycle.c",
        "src/core/target/aarch64.c",
        "src/core/target/x86_64.c",
    ];
    sources
        .into_iter()
        .filter(|source| !included.contains(source))
        .filter(|source| !special.contains(&source.to_string_lossy().as_ref()))
        .filter(|source| {
            let value = source.to_string_lossy();
            if target_os == "macos" {
                !value.starts_with("src/host/linux/")
            } else {
                !value.starts_with("src/host/macos/")
            }
        })
        .map(|source| source.to_string_lossy().into_owned())
        .collect()
}

fn collect_c_sources(root: &Path, directory: &Path, output: &mut BTreeSet<PathBuf>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("enumerate {}: {error}", directory.display()));
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_c_sources(root, &path, output);
        } else if path.extension().and_then(|value| value.to_str()) == Some("c") {
            output.insert(
                path.strip_prefix(root.parent().expect("src parent"))
                    .expect("package source")
                    .to_owned(),
            );
        }
    }
}

fn included_c_sources(source: &Path) -> Vec<PathBuf> {
    let text = fs::read_to_string(source).unwrap_or_else(|error| panic!("read {}: {error}", source.display()));
    text.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let include = line.strip_prefix("#include \"")?.split('"').next()?;
            include.ends_with(".c").then(|| {
                let relative = source.parent().expect("source parent").join(include);
                relative
                    .canonicalize()
                    .unwrap_or_else(|error| panic!("resolve C include {include} from {}: {error}", source.display()))
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
        .include("src/include")
        .include("src")
        .opt_level(2)
        .debug(true)
        .pic(false)
        .warnings(strict)
        .std("c11")
        .flag_if_supported("-fvisibility=hidden")
        .flag_if_supported("-fno-function-sections")
        .flag_if_supported("-fno-data-sections");
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
