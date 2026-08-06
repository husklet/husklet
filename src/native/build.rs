use std::path::{Path, PathBuf};

#[derive(Default)]
struct NativeInputs {
    sources: Vec<PathBuf>,
    assembly: Vec<PathBuf>,
    dependencies: Vec<PathBuf>,
}

impl NativeInputs {
    fn discover(root: &Path) -> Self {
        let mut inputs = Self::default();
        inputs.visit(&root.join("src"), true);
        inputs.visit(&root.join("cache"), true);
        inputs.visit(&root.join("include"), false);
        // cpu.h crosses the native boundary through this crate-owned ABI header.
        inputs.visit(Path::new("cpu/include"), false);
        inputs.sources.sort();
        inputs.assembly.sort();
        inputs.dependencies.sort();
        inputs.dependencies.dedup();
        inputs
    }

    fn visit(&mut self, directory: &Path, compile: bool) {
        let mut entries = std::fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
            .map(|entry| entry.expect("read native input").path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                self.visit(&path, compile);
                continue;
            }
            let extension = path.extension().and_then(|value| value.to_str());
            if !matches!(extension, Some("c" | "h" | "S")) {
                continue;
            }
            self.dependencies.push(path.clone());
            if compile && extension == Some("c") {
                self.sources.push(path);
            } else if compile && extension == Some("S") {
                self.assembly.push(path);
            }
        }
    }
}

// The C test harness in `tests/exec_c.rs` links against the archive this script produces, so the
// archive path, include roots and host architecture travel to it as compile-time environment values.
fn emit_test_environment(root: &Path, allocation_test: bool) {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("output directory"));
    let display = |path: PathBuf| path.to_str().expect("utf-8 native path").to_owned();
    println!("cargo:rustc-env=HL_NATIVE_TEST_ARCHIVE={}", display(out.join("libhl_native_execution.a")));
    println!("cargo:rustc-env=HL_NATIVE_TEST_SOURCES={}", display(manifest.join(root).join("test")));
    println!(
        "cargo:rustc-env=HL_NATIVE_TEST_INCLUDES={}",
        [root.join("include"), root.join("src"), PathBuf::from("cpu/include")]
            .into_iter()
            .map(|path| display(manifest.join(path)))
            .collect::<Vec<_>>()
            .join(":")
    );
    println!(
        "cargo:rustc-env=HL_NATIVE_TEST_ARCH={}",
        std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default()
    );
    // The allocation-test archive interposes malloc, so the ordinary C tests are not linkable against it.
    println!("cargo:rustc-env=HL_NATIVE_TEST_ALLOCATION={}", u8::from(allocation_test));
    println!("cargo:rerun-if-changed={}", root.join("test").display());
}

fn main() {
    let root = Path::new("exec");
    let inputs = NativeInputs::discover(root);
    let mut build = cc::Build::new();
    let allocation_test = std::env::var_os("HL_NATIVE_ALLOCATION_TEST").is_some();
    if allocation_test {
        build.file(root.join("test/allocation.c")).flag("-include").flag(
            root.join("test/allocation.h")
                .to_str()
                .expect("allocation test include"),
        );
        println!("cargo:rerun-if-env-changed=HL_NATIVE_ALLOCATION_TEST");
    }
    // Hardened libc headers reject `_FORTIFY_SOURCE` at `-O0`, where it cannot
    // provide fortification. Preserve Cargo's debug semantics and warning
    // strictness by removing only that unusable definition before libc headers.
    if std::env::var("OPT_LEVEL").as_deref() == Ok("0")
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
    {
        build
            .flag("-include")
            .flag(root.join("include/toolchain.h").to_str().expect("native include path"));
    }
    build
        .files(&inputs.sources)
        .include(root.join("include"))
        .include(root.join("src"))
        .std("c11")
        .warnings(true)
        .extra_warnings(true)
        .flag_if_supported("-Werror");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") && !allocation_test {
        build.files(
            inputs
                .assembly
                .iter()
                .filter(|path| path.ends_with("arch/aarch64/entry.S") || path.ends_with("arch/x86_64/entry.S")),
        );
    }
    build.compile("hl_native_execution");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") && allocation_test {
        let mut assembly = cc::Build::new();
        assembly
            .files(
                inputs
                    .assembly
                    .iter()
                    .filter(|path| path.ends_with("arch/aarch64/entry.S") || path.ends_with("arch/x86_64/entry.S")),
            )
            .include(root.join("include"))
            .include(root.join("src"))
            .warnings(true)
            .extra_warnings(true)
            .flag_if_supported("-Werror")
            .compile("hl_native_execution_entry");
    }
    emit_test_environment(root, allocation_test);
    for dependency in &inputs.dependencies {
        println!("cargo:rerun-if-changed={}", dependency.display());
    }
    // Directory watches discover newly added compilation units and headers.
    for directory in [root.join("src"), root.join("cache"), root.join("include")]
        .into_iter()
        .chain([PathBuf::from("cpu/include")])
    {
        println!("cargo:rerun-if-changed={}", directory.display());
    }
}
