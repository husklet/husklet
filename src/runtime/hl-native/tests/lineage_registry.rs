use std::{fs, path::Path, process::Command};

const PROBE: &str = r#"
#include <errno.h>
#include <stdint.h>
#include <stdlib.h>

#include "linux_abi/container/ownership/lineage_registry.h"

int main(int argc, char **argv) {
    if (argc != 4) return 120;
    uint32_t scenario = (uint32_t)strtoul(argv[1], NULL, 10);
    uint64_t capacity = strtoull(argv[2], NULL, 10);
    uint64_t iterations = strtoull(argv[3], NULL, 10);
    return hl_lineage_registry_fixture(scenario, capacity, iterations);
}
"#;

fn compile(directory: &Path, mutation: Option<&str>) -> std::path::PathBuf {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let probe = directory.join("probe.c");
    let executable = directory.join(mutation.unwrap_or("canonical"));
    fs::write(&probe, PROBE).expect("write lineage-registry probe");
    let mut command = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()));
    command
        .args([
            "-std=c11",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-DHL_LINEAGE_TEST_HOOKS=1",
        ])
        .arg(format!("-I{}", native.display()));
    if let Some(mutation) = mutation {
        command.arg(if mutation.contains('=') {
            format!("-D{mutation}")
        } else {
            format!("-D{mutation}=1")
        });
    }
    let output = command
        .arg(&probe)
        .arg(native.join("linux_abi/container/ownership/lineage_registry.c"))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("compile lineage-registry probe");
    assert!(
        output.status.success(),
        "lineage-registry probe did not compile:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    executable
}

fn run(executable: &Path, scenario: u32, capacity: u64, iterations: u64) -> bool {
    Command::new(executable)
        .args([scenario.to_string(), capacity.to_string(), iterations.to_string()])
        .status()
        .expect("run lineage-registry probe")
        .success()
}

#[test]
fn sparse_generational_lineage_registry_is_bounded_and_reusable() {
    let directory = tempfile::tempdir().expect("lineage-registry scratch directory");
    let executable = compile(directory.path(), None);
    for scenario in 0..=17 {
        if scenario == 3 {
            continue;
        }
        assert!(
            run(&executable, scenario, 64, 0),
            "lineage registry scenario {scenario} failed"
        );
    }
    assert!(
        run(&executable, 3, 1 << 20, (1 << 20) + 1,),
        "production-capacity registry did not survive more than 2^20 create/reclaim cycles"
    );
}

#[test]
fn lineage_registry_disabling_mutations_red_named_scenarios() {
    let directory = tempfile::tempdir().expect("lineage-registry mutation scratch directory");
    for (mutation, scenario) in [
        ("HL_LINEAGE_MUTATE_STOP_AT_TOMBSTONE", 14),
        ("HL_LINEAGE_MUTATE_SKIP_GENERATION_RECHECK", 5),
        ("HL_LINEAGE_MUTATE_SKIP_IDENTITY_RECHECK", 7),
        ("HL_LINEAGE_MUTATE_ACCEPT_STALE_TOKEN", 1),
        ("HL_LINEAGE_MUTATE_IGNORE_REFERENCE_INDEX=0", 8),
        ("HL_LINEAGE_MUTATE_IGNORE_REFERENCE_INDEX=1", 8),
        ("HL_LINEAGE_MUTATE_IGNORE_REFERENCE_INDEX=2", 8),
        ("HL_LINEAGE_MUTATE_IGNORE_REFERENCE_INDEX=3", 8),
        ("HL_LINEAGE_MUTATE_IGNORE_REFERENCE_INDEX=4", 8),
        ("HL_LINEAGE_MUTATE_SKIP_CURSOR_ADVANCE", 10),
        ("HL_LINEAGE_MUTATE_SKIP_OCCUPIED_TRANSITION", 11),
        ("HL_LINEAGE_MUTATE_SKIP_TOMBSTONE_TRANSITION", 11),
        ("HL_LINEAGE_MUTATE_DISABLE_QUOTA", 2),
        ("HL_LINEAGE_MUTATE_ALLOW_GENERATION_WRAP", 6),
        ("HL_LINEAGE_MUTATE_SKIP_RECOVERY_COUNTERS", 16),
        ("HL_LINEAGE_MUTATE_REUSE_REVISION", 17),
        ("HL_LINEAGE_MUTATE_REPLACE_WITHOUT_EXPECTED", 15),
    ] {
        let executable = compile(directory.path(), Some(mutation));
        assert!(
            !run(&executable, scenario, 64, 0),
            "disabling mutation {mutation} did not red scenario {scenario}"
        );
    }
}
