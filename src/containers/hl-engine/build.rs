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
        // cpu.h crosses the native boundary through this project-owned ABI header.
        inputs.visit(Path::new("../../native/cpu/include"), false);
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

fn main() {
    let root = Path::new("../../native/exec");
    let inputs = NativeInputs::discover(root);
    let mut build = cc::Build::new();
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
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
        build.files(
            inputs
                .assembly
                .iter()
                .filter(|path| path.ends_with("arch/aarch64/entry.S") || path.ends_with("arch/x86_64/entry.S")),
        );
    }
    build.compile("hl_native_execution");
    for dependency in &inputs.dependencies {
        println!("cargo:rerun-if-changed={}", dependency.display());
    }
    // Directory watches discover newly added compilation units and headers.
    for directory in [root.join("src"), root.join("cache"), root.join("include")]
        .into_iter()
        .chain([PathBuf::from("../../native/cpu/include")])
    {
        println!("cargo:rerun-if-changed={}", directory.display());
    }
}
