use std::path::{Path, PathBuf};

#[derive(Default)]
struct NativeInputs {
    sources: Vec<PathBuf>,
    assembly: Vec<PathBuf>,
    dependencies: Vec<PathBuf>,
}

fn visit(directory: &Path, inputs: &mut NativeInputs, compile: bool) {
    let mut entries = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .map(|entry| entry.expect("read native input").path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            visit(&path, inputs, compile);
            continue;
        }
        let extension = path.extension().and_then(|value| value.to_str());
        if !matches!(extension, Some("c" | "h" | "S")) {
            continue;
        }
        inputs.dependencies.push(path.clone());
        if compile && extension == Some("c") {
            inputs.sources.push(path);
        } else if compile && extension == Some("S") {
            inputs.assembly.push(path);
        }
    }
}

fn inputs(root: &Path) -> NativeInputs {
    let mut inputs = NativeInputs::default();
    visit(&root.join("src"), &mut inputs, true);
    visit(&root.join("cache"), &mut inputs, true);
    visit(&root.join("include"), &mut inputs, false);
    // cpu.h crosses the native boundary through this project-owned ABI header.
    visit(Path::new("../../schema/cpu/include"), &mut inputs, false);
    inputs.sources.sort();
    inputs.assembly.sort();
    inputs.dependencies.sort();
    inputs.dependencies.dedup();
    inputs
}

fn main() {
    let root = Path::new("../../native/execution");
    let inputs = inputs(root);
    let mut build = cc::Build::new();
    build
        .files(&inputs.sources)
        .include(root.join("include"))
        .include(root.join("src"))
        .std("c11")
        .warnings(true)
        .extra_warnings(true)
        .flag_if_supported("-Werror");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
        build.files(inputs.assembly.iter().filter(|path| {
            path.ends_with("arch/aarch64/entry.S") || path.ends_with("arch/x86_64/entry.S")
        }));
    }
    build.compile("hl_native_execution");
    for dependency in &inputs.dependencies {
        println!("cargo:rerun-if-changed={}", dependency.display());
    }
    // Directory watches discover newly added compilation units and headers.
    for directory in [root.join("src"), root.join("cache"), root.join("include")]
        .into_iter()
        .chain([PathBuf::from("../../schema/cpu/include")])
    {
        println!("cargo:rerun-if-changed={}", directory.display());
    }
}
