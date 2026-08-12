use std::{fs, path::Path, process::Command};

const RETAINED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../runtime/hl-native");
const INTERPRETER_SHA256: &str = "040ace03601d91e7bd3871dd2cd8ca5382790131bbf683b1ca31110b362961ec";
const DISPATCH_SHA256: &str = "fe66d0be5e2aa2dd9a3282e533338eb2f5c9b5ac343775ac4d19d60337f5184f";
const X86_DECODE_SHA256: &str = "1ba08d646d28f03b32c0455801a111c4d2492480e346ba9cca8e80bdcafb88b0";
const X86_DISPATCH_SHA256: &str = "f7d4804c5fb284d2f789835b58299529db7419f48f8e7291f7023b9eae73edc5";

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
fn imported_x86_closure_is_pinned() {
    let retained = Path::new(RETAINED);
    assert_eq!(
        sha256(&retained.join("src/translator/guest/x86_64/decode.c")),
        X86_DECODE_SHA256
    );
    assert_eq!(
        sha256(&retained.join("src/translator/guest/x86_64/interp_dispatch.h")),
        X86_DISPATCH_SHA256
    );
    for path in [
        "src/core/target/x86_64.c",
        "src/linux_abi/x86.c",
        "src/translator/guest/x86_64/decode.c",
        "src/translator/guest/x86_64/interp_dispatch.h",
        "src/translator/host/x86_asm.h",
    ] {
        assert!(retained.join(path).is_file(), "imported source is missing: {path}");
    }
    let target = fs::read_to_string(retained.join("src/core/target/x86_64.c")).expect("x86 target unity");
    assert!(target.contains("translator/guest/x86_64"));
}

#[test]
fn imported_interpreter_is_pinned_and_licensed() {
    let retained = Path::new(RETAINED);
    let interpreter = retained.join("src/translator/guest/aarch64/interp.c");
    let dispatch = retained.join("src/translator/guest/aarch64/interp_dispatch.h");
    assert_eq!(sha256(&interpreter), INTERPRETER_SHA256);
    assert_eq!(sha256(&dispatch), DISPATCH_SHA256);

    let license = fs::read_to_string(retained.join("LICENSE")).expect("retained license");
    assert!(license.starts_with("MIT License\n"));
    assert!(license.contains("Copyright (c) 2026 Richard Huttar"));

    for path in [
        "src/translator/guest/aarch64/interp.c",
        "src/translator/guest/aarch64/interp_dispatch.h",
    ] {
        assert!(retained.join(path).is_file(), "imported source is missing: {path}");
    }
    let target = fs::read_to_string(retained.join("src/core/target/aarch64.c")).expect("A64 target unity");
    assert!(target.contains("#include \"../../translator/guest/aarch64/interp.c\""));
    assert!(target.contains("!defined(HL_A64_INTERPRETER_SMOKE)"));
}
