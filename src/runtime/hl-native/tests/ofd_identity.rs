use std::{fs, path::Path, process::Command};

#[test]
fn both_guest_isa_objects_use_the_shared_nonwrapping_ofd_namespace() {
    for isa in [1, 2] {
        for scenario in 0..=5 {
            hl_native::ofd_identity_test(isa, scenario)
                .unwrap_or_else(|status| panic!("ISA {isa} OFD identity scenario {scenario} failed: {status}"));
        }
    }
}

const PROBE: &str = r#"
#include <stdint.h>
#include <stdlib.h>
#include "linux_abi/container/ownership/ofd_identity.h"
int main(int argc, char **argv) {
    if (argc != 2) return 120;
    return hl_ofd_identity_fixture((uint32_t)strtoul(argv[1], NULL, 10));
}
"#;

fn compile(directory: &Path, mutation: &str) -> std::path::PathBuf {
    let native = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/native");
    let source = directory.join(format!("{mutation}.c"));
    let executable = directory.join(mutation);
    fs::write(&source, PROBE).expect("write OFD identity probe");
    let output = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args([
            "-std=c11",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-DHL_NATIVE_TEST_HOOKS=1",
        ])
        .arg(format!("-D{mutation}=1"))
        .arg(format!("-I{}", native.display()))
        .arg(&source)
        .arg(native.join("linux_abi/container/ownership/ofd_identity.c"))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("compile OFD identity mutation probe");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    executable
}

#[test]
fn disabling_identity_components_and_exhaustion_checks_red_named_scenarios() {
    let directory = tempfile::tempdir().expect("OFD identity mutation scratch");
    for (mutation, scenario) in [
        ("HL_OFD_MUTATE_DROP_LINEAGE", 0),
        ("HL_OFD_MUTATE_DROP_MEMBER", 0),
        ("HL_OFD_MUTATE_ACCEPT_STALE_LINEAGE", 3),
        ("HL_OFD_MUTATE_ALLOW_WRAP", 4),
        ("HL_OFD_MUTATE_USE_GENERATION_AS_LINEAGE", 5),
    ] {
        let executable = compile(directory.path(), mutation);
        assert!(
            !Command::new(executable)
                .arg(scenario.to_string())
                .status()
                .expect("run OFD identity mutation probe")
                .success(),
            "disabling {mutation} did not red scenario {scenario}"
        );
    }
}
