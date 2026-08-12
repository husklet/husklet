use std::{env, fs, path::PathBuf};

#[path = "src/platform.rs"]
mod platform;

const NATIVE_ROOT: &str = ".";
const TU_MANIFEST: &str = "COMPILED_TUS.tsv";
const SOURCE_MANIFEST: &str = "RUNTIME_SOURCES.manifest";

#[derive(Debug)]
struct TranslationUnit<'a> {
    group: &'a str,
    source: &'a str,
    definitions: &'a str,
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=HL_C_SANITIZER");
    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo supplies CARGO_CFG_TARGET_OS");
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo supplies CARGO_CFG_TARGET_ARCH");
    if !platform::supported(&target_os, &target_arch) {
        println!("cargo:warning=native C engine unavailable for {target_arch}-{target_os}");
        return;
    }

    for source in [
        "src/shim.c",
        "src/executable_authority.c",
        "src/executable_authority.h",
        "src/address_projection.c",
        "src/address_projection.h",
        TU_MANIFEST,
        SOURCE_MANIFEST,
    ] {
        println!("cargo:rerun-if-changed={source}");
    }
    let source_manifest = fs::read_to_string(SOURCE_MANIFEST).expect("read native C source manifest");
    for source in source_manifest.lines().filter(|line| !line.is_empty()) {
        println!("cargo:rerun-if-changed={source}");
    }
    let manifest = fs::read_to_string(TU_MANIFEST).expect("read native C translation-unit manifest");
    let units = parse_manifest(&manifest);
    for unit in &units {
        println!("cargo:rerun-if-changed={}", unit.source);
    }

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
            assert_eq!(
                columns.next(),
                Some("include"),
                "native include directory must be package-relative"
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
    assert!(!selected.is_empty(), "native C group {group} is empty");
    let definitions = selected[0].definitions;
    assert!(
        selected.iter().all(|unit| unit.definitions == definitions),
        "native C group {group} has inconsistent definitions"
    );
    assert!(
        definitions.split(';').any(|value| value == "HL_ENABLE_LOGGING=0"),
        "native C group {group} must not write host diagnostics into inherited guest stderr"
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
        .include(NATIVE_ROOT)
        .include("include")
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
