use std::{fs, path::Path, process::Command};

#[test]
fn both_guest_isa_objects_use_the_shared_nonwrapping_ofd_namespace() {
    for isa in [1, 2] {
        for scenario in 0..=13 {
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

const EXEC_PROBE: &str = r#"
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#include "linux_abi/container/ownership/ofd_identity.h"

static uint64_t number(const char *value) { return strtoull(value, NULL, 16); }

int main(int argc, char **argv) {
    const hl_ofd_lineage lineage = {0x123456789abcdef0ULL, 0xfedcba9876543210ULL};
    if (argc == 1) {
        char path[] = "/tmp/hl-ofd-exec-XXXXXX";
        int fd = mkstemp(path);
        if (fd < 0 || unlink(path) != 0 || ftruncate(fd, sizeof(hl_ofd_namespace)) != 0) return 1;
        hl_ofd_namespace *space = mmap(NULL, sizeof *space, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        hl_ofd_generation_binding generation = {11, 13, 1, lineage, 2, 1};
        hl_ofd_member member;
        hl_ofd_identity identity;
        if (space == MAP_FAILED || hl_ofd_namespace_init(space, sizeof *space, lineage, 1) != 0 ||
            hl_ofd_namespace_admit_validated(space, generation) != 0 ||
            hl_ofd_member_bind(&member, space, lineage, 1) != 0 || hl_ofd_identity_mint(&member, &identity) != 0 ||
            fcntl(fd, F_SETFD, 0) != 0)
            return 2;
        char fd_text[24], high[24], low[24], creator[24], sequence[24];
        snprintf(fd_text, sizeof fd_text, "%x", fd);
        snprintf(high, sizeof high, "%llx", (unsigned long long)identity.lineage.high);
        snprintf(low, sizeof low, "%llx", (unsigned long long)identity.lineage.low);
        snprintf(creator, sizeof creator, "%llx", (unsigned long long)identity.member);
        snprintf(sequence, sizeof sequence, "%llx", (unsigned long long)identity.sequence);
        char *next[] = {argv[0], fd_text, high, low, creator, sequence, NULL};
        execv(argv[0], next);
        return 3;
    }
    if (argc != 6) return 4;
    int fd = (int)strtol(argv[1], NULL, 16);
    hl_ofd_identity restored = {{number(argv[2]), number(argv[3])}, number(argv[4]), number(argv[5])};
    hl_ofd_namespace *space = mmap(NULL, sizeof *space, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    hl_ofd_generation_binding generation = {17, 19, 2, lineage, 2, restored.sequence + 1};
    hl_ofd_member member;
    hl_ofd_identity after;
    if (space == MAP_FAILED || hl_ofd_namespace_init(space, sizeof *space, lineage, 0) != 0 ||
        hl_ofd_namespace_admit_validated(space, generation) != 0 ||
        hl_ofd_member_bind(&member, space, lineage, 1) != 0 ||
        hl_ofd_identity_reattach(&member, restored) != 0 || hl_ofd_identity_mint(&member, &after) != 0)
        return 5;
    return hl_ofd_identity_equal(restored, restored) && after.sequence > restored.sequence &&
                   after.member == restored.member && after.lineage.high == restored.lineage.high &&
                   after.lineage.low == restored.lineage.low &&
                   hl_ofd_namespace_init(space, sizeof *space, lineage, 1) == EALREADY
               ? 0
               : 6;
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
        ("HL_OFD_MUTATE_SKIP_REATTACH", 2),
        ("HL_OFD_MUTATE_ALLOW_WRAP", 4),
        ("HL_OFD_MUTATE_USE_GENERATION_AS_LINEAGE", 5),
        ("HL_OFD_MUTATE_ALLOW_LIVE_RESET", 6),
        ("HL_OFD_MUTATE_ACCEPT_REPLAY", 7),
        ("HL_OFD_MUTATE_ALLOW_MEMBER_WRAP", 8),
        ("HL_OFD_MUTATE_SKIP_MEMBER_HIGH_WATER", 9),
        ("HL_OFD_MUTATE_ACCEPT_COLLISION", 10),
        ("HL_OFD_MUTATE_ACCEPT_MEMBER_ROLLBACK", 11),
        ("HL_OFD_MUTATE_ACCEPT_SEQUENCE_ROLLBACK", 12),
        ("HL_OFD_MUTATE_SKIP_PREFLIGHT_IDENTITY", 13),
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

#[test]
fn fd_backed_namespace_reopens_after_exec_and_preserves_restored_identity() {
    let directory = tempfile::tempdir().expect("OFD exec scratch");
    let native = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/native");
    let source = directory.path().join("exec.c");
    let executable = directory.path().join("exec");
    fs::write(&source, EXEC_PROBE).expect("write OFD exec probe");
    let output = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=gnu11", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(&source)
        .arg(native.join("linux_abi/container/ownership/ofd_identity.c"))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("compile OFD exec probe");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(Command::new(executable).status().expect("run OFD exec probe").success());
}
