//! regression: a DYNAMIC non-PIE (ET_EXEC) executable's runtime symbol/address introspection
//! (`dladdr`, `dlsym(RTLD_NEXT/RTLD_DEFAULT, …)`) must match native.
//!
//! The engine maps a non-PIE image HIGH (+bias) but keeps guest-visible addresses at their LOW link
//! value. The auxv AT_PHDR/AT_ENTRY must be LOW too, so the guest ld.so builds the main link_map with
//! l_addr==0 / LOW ranges. A HIGH AT_PHDR set l_addr==bias / HIGH ranges, so the LOW query addresses in
//! `_dl_find_dso_for_object` never matched — `dladdr()` returned 0 and `dlsym(RTLD_NEXT,…)` returned NULL.
//! That is what made clickhouse's sanitizer `dl_iterate_phdr` interceptor throw and recurse until the
//! guest stack overflowed. This needs a real ld.so (dynamic linking), so it runs with an assembled
//! minimal rootfs; it is aarch64-only (the bug is in the aarch64 non-PIE loader path) and Linux-only
//! (needs gcc + the host glibc to assemble the rootfs).

use std::path::PathBuf;
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Run a shell script mac-side (where the ENGINE runs): directly on macOS; through the `mac` bridge on a
/// Linux dev host.
fn mac_sh(script: &str, timeout_s: u32) -> std::process::Output {
    let mut c = Command::new("timeout");
    c.arg(timeout_s.to_string());
    if cfg!(target_os = "macos") {
        c.arg("bash");
    } else {
        c.arg("mac").arg("bash");
    }
    c.arg("-lc").arg(script);
    c.output().expect("spawn mac shell")
}

/// Run a shell script on the HOST that builds the guest (the Linux dev/CI box: gcc/ldd/readelf + native
/// aarch64 execution). Local `bash -c`, never the mac bridge.
fn host_sh(script: &str, timeout_s: u32) -> std::process::Output {
    Command::new("timeout")
        .arg(timeout_s.to_string())
        .arg("bash")
        .arg("-c")
        .arg(script)
        .output()
        .expect("spawn host shell")
}

/// Build the dynamic non-PIE guest and assemble a minimal glibc rootfs (interp + ldd deps) around it, all
/// under the repo tree so the mac-side engine can reach it. Returns (rootfs_dir, in_guest_argv0) or None
/// to SKIP when the toolchain isn't present. Done in one bash script so it runs on the Linux dev/CI box.
fn build_guest_and_rootfs() -> Option<(String, String)> {
    let src = repo().join("dd-tests/guests/nonpie_dladdr.c");
    let work = repo().join("target/dd-tests/nonpie_dladdr");
    let rootfs = work.join("rootfs");
    let guest = work.join("guest");
    let script = format!(
        r#"set -e
        command -v gcc >/dev/null || {{ echo SKIP_NO_GCC; exit 0; }}
        command -v ldd >/dev/null || {{ echo SKIP_NO_LDD; exit 0; }}
        rm -rf {work}; mkdir -p {work} {rootfs}
        gcc -O2 -no-pie -rdynamic -pthread -o {guest} {src} -ldl || {{ echo SKIP_COMPILE; exit 0; }}
        # a dynamic ET_EXEC is required (static -no-pie has no ld.so and can't exercise the bug)
        readelf -h {guest} | grep -q 'Type:.*EXEC' || {{ echo SKIP_NOT_EXEC; exit 0; }}
        readelf -l {guest} | grep -q INTERP || {{ echo SKIP_STATIC; exit 0; }}
        # copy the interpreter (ld.so) preserving its path
        interp=$(readelf -l {guest} | sed -n 's/.*interpreter: \(.*\)]/\1/p' | head -1)
        mkdir -p {rootfs}$(dirname "$interp"); cp -L "$interp" {rootfs}"$interp"
        # copy every ldd dependency preserving its path
        ldd {guest} | awk '/=>/ {{print $3}} !/=>/ {{print $1}}' | grep '^/' | sort -u | while read -r lib; do
            [ -f "$lib" ] || continue; mkdir -p {rootfs}$(dirname "$lib"); cp -L "$lib" {rootfs}"$lib"
        done
        cp {guest} {rootfs}/nonpie_dladdr
        echo OK
        "#,
        work = work.display(),
        rootfs = rootfs.display(),
        guest = guest.display(),
        src = src.display(),
    );
    // The assembly needs gcc/ldd/readelf + ELF aarch64 output — the Linux build host, never the mac side
    // (a real Mac has no ELF cross-toolchain here, so this test simply skips there).
    if cfg!(target_os = "macos") {
        eprintln!("[nonpie_dladdr] no Linux ELF toolchain on macOS — skipping");
        return None;
    }
    let o = host_sh(&script, 120);
    let so = String::from_utf8_lossy(&o.stdout);
    if !so.contains("OK") {
        eprintln!(
            "[nonpie_dladdr] rootfs assembly skipped: {}{}",
            so.trim(),
            String::from_utf8_lossy(&o.stderr).trim()
        );
        return None;
    }
    Some((rootfs.display().to_string(), "/nonpie_dladdr".to_string()))
}

#[test]
fn nonpie_dladdr_rtld_next_aarch64() {
    let guest = ddjit::Guest::LinuxAarch64;
    if !ddjit::available(guest) {
        eprintln!("[nonpie_dladdr] aarch64 engine not built — skipping");
        return;
    }
    let (rootfs, argv0) = match build_guest_and_rootfs() {
        Some(x) => x,
        None => return, // toolchain/rootfs unavailable — skip
    };
    let engine = guest.jit_path().expect("engine path");
    // Native oracle: run the guest directly on the aarch64 Linux build host (chrooted into the rootfs so it
    // uses the same interpreter/libs the engine will).
    let native = host_sh(&format!("{rootfs}{argv0}"), 30);
    let want = String::from_utf8_lossy(&native.stdout).trim().to_string();
    // Native must produce the all-1 result; if the host toolchain is odd, skip rather than false-fail.
    if want != "dladdr=1 sname_ok=1 rtld_next_malloc=1 rtld_default_self=1" {
        eprintln!("[nonpie_dladdr] unexpected native oracle: {want:?} — skipping");
        return;
    }
    let got = mac_sh(&format!("'{engine}' --rootfs '{rootfs}' {argv0}"), 40);
    let got_s = String::from_utf8_lossy(&got.stdout).trim().to_string();
    assert_eq!(
        got_s,
        want,
        "non-PIE dladdr/RTLD_NEXT under the engine must match native (regression #281).\n\
         engine stderr: {}",
        String::from_utf8_lossy(&got.stderr)
    );
}
