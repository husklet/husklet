use std::{collections::BTreeSet, fs, path::Path, process::Command};

const RETAINED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../runtime/native/retained");
const INTERPRETER_SHA256: &str = "040ace03601d91e7bd3871dd2cd8ca5382790131bbf683b1ca31110b362961ec";
const DISPATCH_SHA256: &str = "fe66d0be5e2aa2dd9a3282e533338eb2f5c9b5ac343775ac4d19d60337f5184f";

fn sha256(path: &Path) -> String {
    let output = Command::new("sha256sum").arg(path).output().expect("run sha256sum");
    assert!(output.status.success(), "sha256sum failed for {}", path.display());
    String::from_utf8(output.stdout)
        .expect("sha256 output")
        .split_whitespace()
        .next()
        .expect("sha256 digest")
        .to_owned()
}

#[test]
fn imported_interpreter_is_pinned_licensed_and_inventoried() {
    let retained = Path::new(RETAINED);
    let interpreter = retained.join("src/translator/guest/aarch64/interp.c");
    let dispatch = retained.join("src/translator/guest/aarch64/interp_dispatch.h");
    assert_eq!(sha256(&interpreter), INTERPRETER_SHA256);
    assert_eq!(sha256(&dispatch), DISPATCH_SHA256);

    let license = fs::read_to_string(retained.join("LICENSE")).expect("retained license");
    assert!(license.starts_with("MIT License\n"));
    assert!(license.contains("Copyright (c) 2026 Richard Huttar"));

    let sources = fs::read_to_string(retained.join("RUNTIME_SOURCES.manifest")).expect("source manifest");
    let sources = sources.lines().collect::<BTreeSet<_>>();
    for path in [
        "src/translator/guest/aarch64/interp.c",
        "src/translator/guest/aarch64/interp_dispatch.h",
        "tests/aarch64_interpreter_link_smoke.c",
    ] {
        assert!(sources.contains(path), "source inventory omitted {path}");
    }

    let units = fs::read_to_string(retained.join("COMPILED_TUS.tsv")).expect("TU manifest");
    assert!(units.lines().any(|line| {
        line.starts_with("unity_include\t") && line.contains("src/translator/guest/aarch64/interp.c")
    }));
    assert!(units.lines().any(|line| {
        line.starts_with("interpreter_link_smoke\t") && line.contains("tests/aarch64_interpreter_link_smoke.c")
    }));
    let target = fs::read_to_string(retained.join("src/core/target/aarch64.c")).expect("A64 target unity");
    assert!(target.contains("#include \"../../translator/guest/aarch64/interp.c\""));
    assert!(target.contains("!defined(HL_A64_INTERPRETER_SMOKE)"));
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
fn imported_interpreter_unity_compiles_and_links_unselected() {
    let retained = Path::new(RETAINED);
    let native = retained.parent().expect("native source root");
    let units = fs::read_to_string(retained.join("COMPILED_TUS.tsv")).expect("TU manifest");
    let output = std::env::temp_dir().join(format!("husklet-a64-interpreter-smoke-{}", std::process::id()));
    let mut command = Command::new("cc");
    command
        .arg("-std=c11")
        .arg("-O2")
        .arg("-fno-pie")
        .arg("-no-pie")
        .arg("-I")
        .arg(retained.join("include"))
        .arg("-I")
        .arg(retained.join("src"))
        .arg("-I")
        .arg(native)
        .arg("-D_GNU_SOURCE")
        .arg("-DHL_ENABLE_LOGGING=0")
        .arg("-DHL_TRANSLIT_DEFAULT=0")
        .arg("-DHL_ENGINE_NO_MAIN=1")
        .arg("-DHL_ENGINE_NO_STANDALONE=1")
        .arg("-DHL_A64_INTERPRETER_SMOKE=1");
    for line in units.lines().skip(1) {
        let columns = line.split('\t').collect::<Vec<_>>();
        if columns.first() == Some(&"normal_archive") {
            command.arg(retained.join(columns[2]));
        }
    }
    command
        .arg(native.join("address_projection.c"))
        .arg(retained.join("src/core/target/aarch64.c"))
        .arg(retained.join("tests/aarch64_interpreter_link_smoke.c"))
        .args(["-latomic", "-ldl", "-lm", "-lpthread", "-o"])
        .arg(&output);
    let result = command.output().expect("run retained interpreter smoke link");
    assert!(
        result.status.success(),
        "interpreter smoke link failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let status = Command::new(&output).status().expect("run interpreter smoke");
    assert!(status.success(), "interpreter smoke returned {status}");
    fs::remove_file(output).expect("remove interpreter smoke");
}
