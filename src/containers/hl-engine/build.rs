use std::{env, fs, path::PathBuf};

#[path = "src/retained_platform.rs"]
mod retained_platform;

const C_ENGINE: &str = "../../runtime/native";
const RETAINED: &str = "../../runtime/native/retained";
const TU_MANIFEST: &str = "../../runtime/native/retained/COMPILED_TUS.tsv";
const SOURCE_MANIFEST: &str = "../../runtime/native/retained/RUNTIME_SOURCES.manifest";

#[derive(Debug)]
struct TranslationUnit<'a> {
    group: &'a str,
    source: &'a str,
    definitions: &'a str,
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_C_EXECUTION");
    println!("cargo:rustc-check-cfg=cfg(hl_retained_c)");
    println!("cargo:rustc-check-cfg=cfg(hl_retained_c_default)");
    if env::var_os("CARGO_FEATURE_C_EXECUTION").is_none() {
        return;
    }

    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo supplies CARGO_CFG_TARGET_OS");
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo supplies CARGO_CFG_TARGET_ARCH");
    if !retained_platform::supported(&target_os, &target_arch) {
        println!("cargo:warning=retained C backend unavailable for {target_arch}-{target_os}; using Rust execution");
        return;
    }
    println!("cargo:rustc-cfg=hl_retained_c");
    if retained_platform::production_default(&target_os, &target_arch) {
        println!("cargo:rustc-cfg=hl_retained_c_default");
    }

    println!("cargo:rerun-if-changed={C_ENGINE}/shim.c");
    println!("cargo:rerun-if-changed={C_ENGINE}/executable_authority.c");
    println!("cargo:rerun-if-changed={C_ENGINE}/executable_authority.h");
    println!("cargo:rerun-if-changed={C_ENGINE}/address_projection.c");
    println!("cargo:rerun-if-changed={C_ENGINE}/address_projection.h");
    println!("cargo:rerun-if-changed={TU_MANIFEST}");
    println!("cargo:rerun-if-changed={SOURCE_MANIFEST}");
    let source_manifest = fs::read_to_string(SOURCE_MANIFEST).expect("read retained C source manifest");
    for source in source_manifest.lines().filter(|line| !line.is_empty()) {
        println!(
            "cargo:rerun-if-changed={}",
            PathBuf::from(RETAINED).join(source).display()
        );
    }
    let manifest = fs::read_to_string(TU_MANIFEST).expect("read retained C translation-unit manifest");
    let units = parse_manifest(&manifest);
    let root = PathBuf::from(RETAINED);
    for unit in &units {
        println!("cargo:rerun-if-changed={}", root.join(unit.source).display());
    }

    let platform_definition = if target_os == "macos" {
        "_DARWIN_C_SOURCE"
    } else {
        "_GNU_SOURCE"
    };
    compile(
        "hl_c_backend_shim",
        &["shim.c", "executable_authority.c", "address_projection.c"],
        &[platform_definition],
        false,
    );
    compile_group("hl_c_backend_runtime", "normal_archive", &units, true, &target_os);
    compile_group(
        "hl_c_backend_target_aarch64",
        "target_aarch64_direct",
        &units,
        false,
        &target_os,
    );
    compile_group(
        "hl_c_backend_target_x86_64",
        "target_x86_64_direct",
        &units,
        false,
        &target_os,
    );
    compile_group(
        "hl_c_backend_lifecycle_aarch64",
        "lifecycle_aarch64_direct",
        &units,
        false,
        &target_os,
    );
    compile_group(
        "hl_c_backend_lifecycle_x86_64",
        "lifecycle_x86_64_direct",
        &units,
        false,
        &target_os,
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
        // Link directives propagate to binaries which depend on hl-engine;
        // rustc-link-arg does not. Whole-archive also resolves the retained
        // engine's intentional circular references without relying on final
        // link-line ordering.
        println!("cargo:rustc-link-lib=static:+whole-archive={archive}");
    }
    let libraries: &[&str] = if target_os == "macos" {
        &["m", "pthread"]
    } else {
        &["atomic", "dl", "m", "pthread"]
    };
    for library in libraries {
        println!("cargo:rustc-link-lib={library}");
    }
}

fn parse_manifest(manifest: &str) -> Vec<TranslationUnit<'_>> {
    manifest
        .lines()
        .skip(1)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut columns = line.split('\t');
            let group = columns.next().expect("translation-unit group");
            let _target = columns.next().expect("translation-unit target");
            let source = columns.next().expect("translation-unit source");
            let definitions = columns.next().expect("translation-unit definitions");
            let include = columns.next().expect("translation-unit include directory");
            assert_eq!(
                include, "include",
                "retained include directory must be repository-relative"
            );
            assert!(columns.next().is_none(), "unexpected translation-unit manifest column");
            TranslationUnit {
                group,
                source,
                definitions,
            }
        })
        .collect()
}

fn compile_group(name: &str, group: &str, units: &[TranslationUnit<'_>], strict: bool, target_os: &str) {
    let selected = units.iter().filter(|unit| unit.group == group).collect::<Vec<_>>();
    assert!(!selected.is_empty(), "retained C group {group} is empty");
    let definitions = selected[0].definitions;
    assert!(
        selected.iter().all(|unit| unit.definitions == definitions),
        "retained C group {group} has inconsistent definitions"
    );
    assert!(
        definitions.split(';').any(|value| value == "HL_ENABLE_LOGGING=0"),
        "retained C group {group} must not write host diagnostics into inherited guest stderr"
    );
    let platform_sources = selected
        .iter()
        .map(|unit| platform_source(unit.source, target_os))
        .collect::<Vec<_>>();
    let sources = platform_sources.iter().map(String::as_str).collect::<Vec<_>>();
    let definitions = definitions
        .split(';')
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    compile(name, &sources, &definitions, strict);
}

fn platform_source(source: &str, target_os: &str) -> String {
    if target_os == "macos"
        && let Some(name) = source.strip_prefix("src/host/linux/")
    {
        return format!("src/host/macos/{name}");
    }
    source.to_owned()
}

fn compile(name: &str, sources: &[&str], definitions: &[&str], strict: bool) {
    let mut build = cc::Build::new();
    build
        .cargo_metadata(false)
        .include(C_ENGINE)
        .include(format!("{RETAINED}/include"))
        .include(format!("{RETAINED}/src"))
        .opt_level(2)
        .debug(true)
        .pic(false)
        .warnings(strict)
        .std("c11")
        .flag_if_supported("-fvisibility=hidden")
        .flag_if_supported("-fno-function-sections")
        .flag_if_supported("-fno-data-sections");
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
        match definition.split_once('=') {
            Some((name, value)) => {
                build.define(name, value);
            }
            None => {
                build.define(definition, None);
            }
        }
    }
    for source in sources {
        let path = if matches!(*source, "shim.c" | "executable_authority.c" | "address_projection.c") {
            PathBuf::from(C_ENGINE).join(source)
        } else {
            PathBuf::from(RETAINED).join(source)
        };
        build.file(path);
    }
    build.compile(name);
}
