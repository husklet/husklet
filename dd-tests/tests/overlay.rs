//! Overlay-correctness differential tests (the "overlay test lane").
//!
//! dd emulates Linux `overlayfs` in userspace (a single read-only lower "image" + a writable upper, unioned
//! by path-rewriting in the JIT). This lane pins that emulation to REAL overlayfs semantics: it builds a
//! synthetic lower layer with known content, launches the `overlay/probe` guest against it on EVERY buildable
//! Linux guest arch (x86_64 AND aarch64 — the two engines that run image overlays), and asserts the guest's
//! observations equal what the Linux kernel's overlayfs would produce. Ground-truth values were checked
//! against `docker`/`mount -t overlay` on Linux (see comments per gap).
//!
//! Platform coverage: overlay (lower/upper union, whiteout, copy-up, opaque) is a LINUX-container feature —
//! it exists only when the engine is launched with `--lower` image layers (`g_nlower>0`). The darwin/aarch64
//! engine (the `ddcli mac` macOS container) runs under darwinjail over the host FS with NO lower layers
//! (`SpawnConfig` never emits `--lower` for the darwin script; `g_nlower==0`), so the entire overlay code
//! path is inert there and has no analogue to test — a macOS container has one plain rootfs, not a union.
//! The image-flattening opaque fix that IS platform-independent (it runs in the daemon at pull time,
//! regardless of guest arch) is covered by the `dd-daemon` unit test `registry::tests::opaque_*`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// Compile the probe static-PIE for a guest arch. Returns the host binary path.
fn compile_probe(arch: &str) -> String {
    let src = repo().join("dd-tests/guests/overlay/probe.c");
    let outdir = repo().join("target/dd-tests").join(arch);
    std::fs::create_dir_all(&outdir).unwrap();
    let out = outdir.join("overlay_probe");
    let cc = if arch == "x86_64" { "x86_64-linux-gnu-gcc" } else { "gcc" };
    let o = Command::new(cc)
        .args(["-O2", "-static-pie", "-pthread", "-o"])
        .arg(&out).arg(&src)
        .output()
        .unwrap_or_else(|e| panic!("{cc} spawn: {e}"));
    assert!(o.status.success(), "compile probe [{arch}]: {}", String::from_utf8_lossy(&o.stderr));
    out.to_string_lossy().into_owned()
}

fn write(p: &Path, data: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, data).unwrap();
}

/// Build a fresh synthetic lower layer (a flattened "image") + an empty upper, both on the shared repo
/// tree (the macOS engine must be able to see them — not /tmp).
fn build_layers(arch: &str) -> (PathBuf, PathBuf) {
    let root = repo().join("target/dd-tests").join(format!("overlay-{arch}"));
    let _ = std::fs::remove_dir_all(&root);
    let lower = root.join("lower");
    let upper = root.join("upper");
    std::fs::create_dir_all(&lower).unwrap();
    std::fs::create_dir_all(&upper).unwrap();

    // G2: a populated lower-only directory (with a nested subdir).
    write(&lower.join("g2src/f1"), b"content-f1");
    write(&lower.join("g2src/f2"), b"content-f2");
    write(&lower.join("g2src/sub/nested"), b"deep");

    // G3: a lower file with setuid mode + a fixed past mtime — copy-up must preserve both.
    let meta = lower.join("meta");
    write(&meta, b"m");
    std::fs::set_permissions(&meta, PermissionsExt::from_mode(0o4755)).unwrap();
    // deterministic mtime (2001-09-09T01:46:40Z); GNU touch's @epoch form.
    let t = Command::new("touch").args(["-d", "@1000000000"]).arg(&meta).output().unwrap();
    assert!(t.status.success(), "touch mtime: {}", String::from_utf8_lossy(&t.stderr));

    // G5: a lower-only file whose setxattr must copy it up and keep its bytes.
    write(&lower.join("xf"), b"xf-lower");

    // G6/G1r: a lower-backed dir with content that must NOT leak after rm+recreate.
    write(&lower.join("opqdir/stale1"), b"s1");
    write(&lower.join("opqdir/stale2"), b"s2");

    // whiteout/unlink semantics: a non-empty lower-backed dir (rmdir must ENOTEMPTY, keep its child).
    write(&lower.join("rmdne/keep"), b"kept");

    // G7: a lower-only file for RENAME_EXCHANGE with an upper-only sibling created by the probe, and a
    // pair of lower-only files to exchange (both ends must copy up).
    write(&lower.join("ex_a"), b"AAA");
    write(&lower.join("ex_c"), b"CCC");
    write(&lower.join("ex_d"), b"DDD");

    // M-series (#169) fixtures: a nested lower-only directory (mkdir/rename must copy up its parents),
    // a lower-only regular file used both as a bad path COMPONENT (ENOTDIR) and as an rmdir-of-a-file
    // target, and a lower-only file removed+recreated-as-dir to exercise mkdir over a whiteout.
    write(&lower.join("mkp/a/b/keep"), b"lower-keep");
    write(&lower.join("mnotdir"), b"i-am-a-file");
    write(&lower.join("wf"), b"wf-lower");
    // a lower-only EMPTY directory (rmdir of it must succeed via whiteout).
    std::fs::create_dir_all(lower.join("mkp_empty")).unwrap();
    // a PRISTINE lower-only directory never touched by the probe — mkdir over it must be EEXIST purely by
    // lower detection (not because a prior copy-up put it in the upper).
    write(&lower.join("ld_pristine/child"), b"c");

    // N-series (#239/#269): a lower-backed directory removed at runtime must HIDE its read-only lower
    // children from a later stat/access (the per-layer resolve used to still find the child through the
    // whited-out parent -> stale positive). rmparent = flat dir; rmdeep = a deep subtree removed
    // bottom-up; rmcopy = a dir with a copied-up child + a lower child, emptied then rmdir'd (#269).
    write(&lower.join("rmparent/c1"), b"one");
    write(&lower.join("rmparent/c2"), b"two");
    write(&lower.join("rmdeep/a/b/c/leaf"), b"deepleaf");
    write(&lower.join("rmcopy/k1"), b"kk");

    (lower, upper)
}

/// Run the probe under the engine for `arch`; return parsed KEY=VALUE map of its stdout.
fn run_probe(arch: &str) -> std::collections::HashMap<String, String> {
    let guest = match arch {
        "x86_64" => ddjit::Guest::LinuxX86_64,
        _ => ddjit::Guest::LinuxAarch64,
    };
    let probe = compile_probe(arch);
    let (lower, upper) = build_layers(arch);
    // The engine resolves argv[0] THROUGH the jail (like a container running a binary from its image), so
    // the probe must be a guest-visible path: drop it into the lower "image" and launch it as `/probe`.
    std::fs::copy(&probe, lower.join("probe")).unwrap();
    std::fs::set_permissions(&lower.join("probe"), PermissionsExt::from_mode(0o755)).unwrap();

    let mut cfg = ddjit::SpawnConfig::new(String::new(), upper.to_string_lossy().into_owned());
    cfg.lowers = vec![lower.to_string_lossy().into_owned()];
    cfg.argv = vec!["/probe".into()];
    let (prog, args) = cfg.command(guest).expect("engine command");
    let out = Command::new("timeout").arg("30").arg(&prog).args(&args).output().expect("spawn engine");
    let stdout = String::from_utf8_lossy(&out.stdout);
    if std::env::var("DD_DEBUG").is_ok() {
        eprintln!("[{arch}] code={:?}\nstdout=\n{stdout}\nstderr=\n{}", out.status.code(),
            String::from_utf8_lossy(&out.stderr));
    }
    assert!(stdout.contains("PROBE_DONE"), "[{arch}] probe did not finish; stdout=\n{stdout}\nstderr=\n{}",
        String::from_utf8_lossy(&out.stderr));
    stdout.lines().filter_map(|l| l.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))).collect()
}

fn check(arch: &str) {
    let m = run_probe(arch);
    let g = |k: &str| m.get(k).cloned().unwrap_or_else(|| format!("<missing {k}>"));

    // G2 — rename of a lower-only populated dir: succeeds, whole subtree moves, source gone.
    assert_eq!(g("g2_rename_ret"), "0", "[{arch}] G2 rename must succeed");
    assert_eq!(g("g2_dst_f1"), "content-f1", "[{arch}] G2 lost /g2dst/f1 (data loss)");
    assert_eq!(g("g2_dst_f2"), "content-f2", "[{arch}] G2 lost /g2dst/f2 (data loss)");
    assert_eq!(g("g2_dst_nested"), "deep", "[{arch}] G2 lost nested subtree (data loss)");
    assert_eq!(g("g2_src_gone"), "1", "[{arch}] G2 source must be gone after rename");

    // G3 — copy-up preserves setuid mode and mtime.
    assert_eq!(g("g3_mode"), "4755", "[{arch}] G3 copy-up must preserve setuid+mode (got {})", g("g3_mode"));
    assert_eq!(g("g3_mtime"), "1000000000", "[{arch}] G3 copy-up must preserve mtime (got {})", g("g3_mtime"));

    // G5 — xattr passthrough round-trips; setxattr on a lower file copies it up and keeps bytes.
    assert_eq!(g("g5_set_ret"), "0", "[{arch}] G5 setxattr must succeed");
    assert_eq!(g("g5_get"), "hello", "[{arch}] G5 getxattr must return what setxattr stored");
    assert_eq!(g("g5_list_has_user_a"), "1", "[{arch}] G5 listxattr must include the set attr");
    assert_eq!(g("g5_copyup_get"), "v1", "[{arch}] G5 setxattr on a lower file must persist after copy-up");
    assert_eq!(g("g5_copyup_bytes"), "xf-lower", "[{arch}] G5 copy-up must preserve the file's bytes");

    // G6 + G1(runtime) — recreate a removed lower-backed dir; lower children must not leak.
    assert_eq!(g("g1r_gone_after_rm"), "1", "[{arch}] G1r: `rm -rf` of a lower-backed dir must remove it entirely (no leftover upper dir)");
    assert_eq!(g("g6_dir_visible"), "1", "[{arch}] G6 recreated dir must be visible (stale .wh. cleared)");
    assert_eq!(g("g1r_readdir"), "fresh", "[{arch}] G1r opaque: readdir must show only new content, not lower stale* (got {})", g("g1r_readdir"));
    assert_eq!(g("g1r_stale_lookup"), "1", "[{arch}] G1r opaque: lower child must not be looked up through the recreated dir");

    // whiteout/unlink: rmdir of a non-empty lower-backed dir must fail ENOTEMPTY and keep its children.
    assert_eq!(g("g4b_rmdir_nonempty"), "1", "[{arch}] rmdir of a non-empty lower-backed dir must fail ENOTEMPTY");
    assert_eq!(g("g4b_child_kept"), "kept", "[{arch}] rmdir ENOTEMPTY must not have removed the lower child");

    // G7 — RENAME_EXCHANGE across layers swaps both ends.
    assert_eq!(g("g7_exchange_ret"), "0", "[{arch}] G7 exchange must succeed");
    assert_eq!(g("g7_a"), "BBB", "[{arch}] G7 exchange: /ex_a must hold the upper's old content");
    assert_eq!(g("g7_b"), "AAA", "[{arch}] G7 exchange: /ex_b must hold the lower's old content");
    assert_eq!(g("g7_exchange2_ret"), "0", "[{arch}] G7 exchange of two lower-only files must succeed");
    assert_eq!(g("g7_c"), "DDD", "[{arch}] G7 exchange (both lower): /ex_c must swap to /ex_d's content");
    assert_eq!(g("g7_d"), "CCC", "[{arch}] G7 exchange (both lower): /ex_d must swap to /ex_c's content");

    // ---- M-series (#169): directory-CREATION ops through the overlay must return the real-overlayfs
    // errno (success / EEXIST / ENOENT / ENOTDIR), never a blanket EPERM and never a spurious success that
    // masks a lower entry. Ground truth from `mount -t overlay` on Linux. ----
    assert_eq!(g("m1_mkdir_new"), "0", "[{arch}] M1 plain mkdir into the upper must succeed (not EPERM)");
    assert_eq!(g("m1_mkdir_again"), "17", "[{arch}] M1 mkdir of an existing upper name must be EEXIST");
    assert_eq!(g("m2_mode"), "755", "[{arch}] M2 mkdir mode must honor umask 022 (0777 -> 0755)");
    assert_eq!(g("m3_mkdirat"), "0", "[{arch}] M3 mkdirat via an explicit dir-fd must succeed");
    assert_eq!(g("m3_visible"), "1", "[{arch}] M3 mkdirat result must be visible");
    assert_eq!(g("m4_nested_lower_parent"), "0", "[{arch}] M4 mkdir under a lower-only parent chain must succeed (copy-up)");
    assert_eq!(g("m4_visible"), "1", "[{arch}] M4 nested mkdir result must be visible");
    assert_eq!(g("m5_enoent"), "1", "[{arch}] M5 mkdir under a missing parent must be ENOENT (got errno {})", g("m5_enoent"));
    assert_eq!(g("m6_enotdir"), "1", "[{arch}] M6 mkdir with a lower-only FILE as an intermediate component must be ENOTDIR (got errno {})", g("m6_enotdir"));
    assert_eq!(g("m7_mkdir_over_wh"), "0", "[{arch}] M7 mkdir over a whiteout (recreate a removed name) must succeed");
    assert_eq!(g("m7_isdir"), "1", "[{arch}] M7 recreated name must be a directory");
    assert_eq!(g("m8_creat"), "0", "[{arch}] M8 creat into an overlay upper dir must succeed");
    assert_eq!(g("m8_symlink"), "0", "[{arch}] M8 symlink into an overlay upper dir must succeed");
    assert_eq!(g("m8_mknod_fifo"), "0", "[{arch}] M8 mknod(FIFO) into an overlay upper dir must succeed");
    assert_eq!(g("m9_rmdir"), "0", "[{arch}] M9 rmdir of an upper-created empty dir must succeed");
    assert_eq!(g("m9_rmdir_gone"), "1", "[{arch}] M9 rmdir of an already-removed dir must be ENOENT");
    assert_eq!(g("m10_rmdir_file"), "1", "[{arch}] M10 rmdir of a regular file must be ENOTDIR (got errno {})", g("m10_rmdir_file"));
    assert_eq!(g("m10_unlinkat_rmdir"), "0", "[{arch}] M10 unlinkat AT_REMOVEDIR of an empty dir must succeed");
    assert_eq!(g("m11_rename_into_overlay"), "0", "[{arch}] M11 rename of an upper file into a lower-only (copied-up) dir must succeed");
    assert_eq!(g("m11_dst"), "REN", "[{arch}] M11 renamed file must be readable at its destination");
    assert_eq!(g("m12_mkdir_over_lowerdir"), "1", "[{arch}] M12 mkdir over a lower-only DIR must be EEXIST (got errno {})", g("m12_mkdir_over_lowerdir"));
    assert_eq!(g("m12_mkdir_over_lowerfile"), "1", "[{arch}] M12 mkdir over a lower-only FILE must be EEXIST, not a spurious success (got errno {})", g("m12_mkdir_over_lowerfile"));
    assert_eq!(g("m13_symlink_eexist"), "1", "[{arch}] M13 symlink over a lower-only name must be EEXIST (got errno {})", g("m13_symlink_eexist"));
    assert_eq!(g("m13_openexcl_eexist"), "1", "[{arch}] M13 open(O_CREAT|O_EXCL) over a lower-only name must be EEXIST (got errno {})", g("m13_openexcl_eexist"));
    assert_eq!(g("m14_rmdir_lower_empty"), "0", "[{arch}] M14 rmdir of a lower-only EMPTY dir must succeed (whiteout)");
    assert_eq!(g("m14_gone"), "1", "[{arch}] M14 rmdir'd lower dir must then be gone");
    assert_eq!(g("m15_mkdirat_lower_dirfd"), "0", "[{arch}] M15 mkdirat under a lower-only (copied-up) dir-fd must succeed");
    assert_eq!(g("m15_visible"), "1", "[{arch}] M15 mkdirat-under-lower-dirfd result must be visible");

    // ---- N-series (#239/#269): after a lower-backed directory is removed, its read-only lower children
    // must be HIDDEN from a later stat/access (no stale positive), and a deep subtree removed bottom-up
    // must leave no resolvable descendant. Ground truth from `docker` on real overlayfs. ----
    assert_eq!(g("n1_parent_gone"), "1", "[{arch}] N1 removed lower-backed dir must be gone");
    assert_eq!(g("n1_child_access"), "1", "[{arch}] #239: access() of a child after its lower-backed dir was rm'd must be ENOENT (stale positive)");
    assert_eq!(g("n1_child_stat"), "1", "[{arch}] #239: stat() of a child after its lower-backed dir was rm'd must be ENOENT (stale positive)");
    assert_eq!(g("n2_leaf_gone"), "1", "[{arch}] #239/#269: deep descendant of a removed lower subtree must not stale-resolve");
    assert_eq!(g("n2_midc_gone"), "1", "[{arch}] #239/#269: mid-level dir of a removed lower subtree must not stale-resolve");
    assert_eq!(g("n2_top_gone"), "1", "[{arch}] N2 removed subtree top must be gone");
    assert_eq!(g("n3_rmdir"), "0", "[{arch}] #269: rmdir of an emptied copied-up lower dir must succeed (not ENOTEMPTY)");
    assert_eq!(g("n3_child_gone"), "1", "[{arch}] #269: child of an rmdir'd copied-up lower dir must be ENOENT afterward");
}

#[test]
fn overlay_correctness_aarch64() {
    if !ddjit::available(ddjit::Guest::LinuxAarch64) {
        eprintln!("linux/aarch64 engine not built — skipping (pin DDJIT_DIR to a built engine)");
        return;
    }
    check("aarch64");
}

#[test]
fn overlay_correctness_x86_64() {
    if !ddjit::available(ddjit::Guest::LinuxX86_64) {
        eprintln!("linux/x86_64 engine not built — skipping (pin DDJIT_DIR to a built engine)");
        return;
    }
    check("x86_64");
}
