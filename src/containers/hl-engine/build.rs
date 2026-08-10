use std::{env, fs, path::PathBuf};

const RETAINED: &str = "c_backend/retained";
const TU_MANIFEST: &str = "c_backend/retained/COMPILED_TUS.tsv";
const SOURCE_MANIFEST: &str = "c_backend/retained/RUNTIME_SOURCES.manifest";

#[derive(Debug)]
struct TranslationUnit<'a> {
    group: &'a str,
    source: &'a str,
    definitions: &'a str,
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_C_EXECUTION");
    if env::var_os("CARGO_FEATURE_C_EXECUTION").is_none() {
        return;
    }

    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo supplies CARGO_CFG_TARGET_OS");
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo supplies CARGO_CFG_TARGET_ARCH");
    assert!(
        target_os == "linux" && target_arch == "aarch64",
        "the retained C execution backend currently supports only Linux/AArch64 (target is {target_arch}-{target_os})"
    );

    println!("cargo:rerun-if-changed=c_backend/shim.c");
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

    compile("hl_c_backend_shim", &["c_backend/shim.c"], &["_GNU_SOURCE"], false);
    compile_group("hl_c_backend_runtime", "normal_archive", &units, true);
    compile_group("hl_c_backend_target", "target_unity_direct", &units, false);
    compile_group("hl_c_backend_lifecycle", "lifecycle_direct", &units, false);

    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"));
    println!("cargo:rustc-link-arg=-Wl,--start-group");
    for archive in [
        "hl_c_backend_shim",
        "hl_c_backend_target",
        "hl_c_backend_lifecycle",
        "hl_c_backend_runtime",
    ] {
        println!(
            "cargo:rustc-link-arg={}",
            output.join(format!("lib{archive}.a")).display()
        );
    }
    println!("cargo:rustc-link-arg=-Wl,--end-group");
    // Rust's native-library directives are placed before late link arguments;
    // repeat the C runtime dependencies here so archive references resolve in
    // the same order as the retained CMake link.
    println!("cargo:rustc-link-arg=-latomic");
    println!("cargo:rustc-link-arg=-lgcc");
    println!("cargo:rustc-link-arg=-lc");
    for library in ["atomic", "dl", "m", "pthread"] {
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

fn compile_group(name: &str, group: &str, units: &[TranslationUnit<'_>], strict: bool) {
    let selected = units.iter().filter(|unit| unit.group == group).collect::<Vec<_>>();
    assert!(!selected.is_empty(), "retained C group {group} is empty");
    let definitions = selected[0].definitions;
    assert!(
        selected.iter().all(|unit| unit.definitions == definitions),
        "retained C group {group} has inconsistent definitions"
    );
    let sources = selected.iter().map(|unit| unit.source).collect::<Vec<_>>();
    let definitions = definitions
        .split(';')
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    compile(name, &sources, &definitions, strict);
}

fn compile(name: &str, sources: &[&str], definitions: &[&str], strict: bool) {
    let mut build = cc::Build::new();
    build
        .cargo_metadata(false)
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
        let path = if source.starts_with("c_backend/") {
            PathBuf::from(source)
        } else {
            PathBuf::from(RETAINED).join(source)
        };
        build.file(path);
    }
    build.compile(name);
}
