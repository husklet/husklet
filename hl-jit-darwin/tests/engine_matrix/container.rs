//! Container behaviour, sandbox containment, and x86-64 / macOS guest fixtures.

use crate::support::{fixture, group, in_rootfs, src, Engine, Group};

use super::sh;

/// Container behaviour — namespaces, fs, limits (alpine, aarch64).
pub(super) fn container() -> Group {
    group("container", vec![
        sh("id-root", "id -u").out("0\n"),                                  // USER-ns: root by default
        sh("uname-m", "uname -m").out("aarch64\n"),
        sh("pwd", "pwd").out("/\n"),
        // write cases clean up after themselves (the image rootfs is shared — keep it pristine).
        sh("mkdir", "mkdir -p /x/y && echo /x/y; rm -rf /x").out("/x/y\n"),
        sh("chmod", "rm -f /f; touch /f && chmod 700 /f && stat -c %a /f; rm -f /f").out("700\n"),
        sh("symlink", "rm -f /l; ln -s /etc/hostname /l && readlink /l; rm -f /l").out("/etc/hostname\n"),
        // #353 regression guard: the daemon's DEFAULT launch is the typed --configfd bridge, which hands the
        // UTS hostname to the engine as the DD_HOSTNAME *env* (hl_configfd.c) — NOT the --hostname CLI flag
        // that the out-of-process SpawnConfig::script path emits. aarch64's container_init() already re-read
        // DD_HOSTNAME; x86-64 dropped it, so `docker run --hostname h` on x86 returned "jit". The flag-only
        // matrix never drove the env path (the coverage gap), so inject DD_HOSTNAME directly and assert the
        // guest's gethostname() sees it — on BOTH Linux engines.
        {
            let mut c = in_rootfs("hostname-env", "alpine", &["/bin/hostname"]);
            c.engines = vec![Engine::LinuxAarch64, Engine::LinuxX86_64];
            c.env.push(("DD_HOSTNAME".to_string(), "ddenvhost".to_string()));
            c
        }
        .out("ddenvhost\n"),
        sh("proc-self", "test -r /proc/self/status && echo proc-ok").out("proc-ok\n"),
        sh("dev-null", "echo discard > /dev/null && echo dev-ok").out("dev-ok\n"),
        // /dev completeness: fd/std* symlinks + ptmx/pts/shm/console nodes the OCI unpacker strips.
        sh("dev-fd-link", "readlink /dev/fd").out("/proc/self/fd\n"),       // the standard symlink
        sh("dev-fd-open", "printf hi | cat /dev/fd/0").out("hi"),            // /dev/fd/N -> reopen host fd
        sh("dev-stdin", "printf yo | cat /dev/stdin").out("yo"),            // /dev/stdin open -> reopen fd 0
        sh("dev-stdin-link", "readlink /dev/stdin").out("/proc/self/fd/0\n"), // readlink keeps symlink text
        sh("dev-present", "for f in fd stdin stdout stderr ptmx shm console pts; do test -e /dev/$f || { echo MISSING $f; exit 1; }; done; echo all-present").out("all-present\n"),
        // `ls -l /dev` must not error (readlink of std* went via the on-disk symlink, not a pipe F_GETPATH).
        sh("dev-ls-clean", "ls -l /dev >/dev/null 2>/tmp/e; wc -l </tmp/e | tr -d ' '; rm -f /tmp/e").out("0\n"),
        sh("mem-ok", "echo cg ok").mem(64 << 20).out("cg ok\n"),            // runs under cgroup limit
    ])
}

/// Sandbox containment — the rootfs jail must not leak the host filesystem.
/// (NB: `..` paths are avoided here — the dev-only orbstack `mac` bridge mangles them; a real macOS
/// host runs the JIT directly. The jail itself is exercised via absolute host paths below.)
pub(super) fn sandbox() -> Group {
    group(
        "sandbox",
        vec![
            // the guest's "/" is the rootfs (has its own bin/etc), not the host root.
            sh(
                "jail-root",
                "test -d /etc && test -d /bin && echo rootfs-root",
            )
            .out("rootfs-root\n"),
            // host-only absolute paths are not present inside the jail -> ENOENT, never the host dir.
            sh("jail-no-users", "cat /Users 2>&1; echo DONE")
                .has("DONE")
                .has("o such file"),
            sh("jail-no-private", "cat /private/etc/hosts 2>&1; echo DONE")
                .has("DONE")
                .has("o such file"),
            // --- untrusted-guest SENTRY split (DDJIT_UNTRUSTED) ---------------------------------------------
            // Each guest is registered TWICE against the SAME golden line: once on the trusted path (baseline)
            // and once with `.untrusted()` (DDJIT_UNTRUSTED=1) so every fs/net syscall is marshaled to the
            // forked sentry over the SPSC ring and the copied-back bytes must reproduce the baseline exactly.
            // `.untrusted()` covers DDJIT_UNTRUSTED (ring only); `.sandbox()` covers the PUBLIC sandbox mode
            // (DDJIT_UNTRUSTED + DDJIT_SANDBOX, the exact combo `docker run --security-opt sandbox` emits) so
            // the public mode is no longer avoided by the matrix — on macOS it drives the deny-default Seatbelt
            // worker confinement, on Linux it pins the combined env still reproduces the trusted golden.
            // fs round-trip: openat/write/lseek/read/pread64/fstat/getdents64/close all cross the ring.
            src("sentry-fs", "sentry_fs.c").out("sentry_fs sum=32640 size=256 found=1\n"),
            src("sentry-fs-untrusted", "sentry_fs.c")
                .out("sentry_fs sum=32640 size=256 found=1\n")
                .untrusted(),
            src("sentry-fs-sandbox", "sentry_fs.c")
                .out("sentry_fs sum=32640 size=256 found=1\n")
                .sandbox(),
            // socket family: socket/bind/getsockname/sendto/recvfrom on a sentry-owned UDP loopback socket.
            src("sentry-net", "sentry_net.c").out("sentry_net echo=datagram-echo-42 len=16\n"),
            src("sentry-net-untrusted", "sentry_net.c")
                .out("sentry_net echo=datagram-echo-42 len=16\n")
                .untrusted(),
            // clone-FORK lane: a single fork() whose CHILD writes a /tmp file on a freshly CAS-claimed lane
            // (sentry_fork_child drops the inherited lane) while the PARENT reaps via wait4 then reads it back
            // on its own lane. Exercises lane-reclaim + the 8-ring pool under two live workers + owner-gated
            // reap -- the riskiest forwarding path. Same golden under the split as trusted (sum == sentry_fs).
            src("sentry-fork", "sentry_fork.c")
                .out("sentry_fork child_exit=7 readback=ok sum=32640\n"),
            src("sentry-fork-untrusted", "sentry_fork.c")
                .out("sentry_fork child_exit=7 readback=ok sum=32640\n")
                .untrusted(),
            // fork lane under the PUBLIC sandbox mode: the child worker is re-confined under Seatbelt after
            // the fork (macOS), so this guards that lane-reclaim + reap survive the confined worker path too.
            src("sentry-fork-sandbox", "sentry_fork.c")
                .out("sentry_fork child_exit=7 readback=ok sum=32640\n")
                .sandbox(),
        ],
    )
}

/// x86-64 guest — prebuilt fixtures through the jit86 engine. The binaries are COMMITTED at
/// hl-jit-darwin/testdata/guests/x86/ next to their sources + build.sh (they pin binary flavors the on-the-fly
/// `src()` builds can't: nolibc raw-syscall _start guests, static-PIE vs static non-PIE glibc
/// startups, static non-PIE Go). They historically lived in a machine-local `<repo-parent>/poc/`
/// sidecar that vanished with that machine — in-repo they run from any fresh checkout or worktree.
pub(super) fn x86() -> Group {
    group(
        "x86",
        vec![
            fixture("hello", &[(Engine::LinuxX86_64, "guests/x86/hello_x86")])
                .exit(42)
                .out("hi\n"),
            fixture("glibc", &[(Engine::LinuxX86_64, "guests/x86/g_x64")]).has("glibc ok"),
            fixture("glibc-min", &[(Engine::LinuxX86_64, "guests/x86/gw")]).has("glibc-min ok"),
            fixture("ctest", &[(Engine::LinuxX86_64, "guests/x86/ctest_x64")]).exit(7),
            fixture("hx", &[(Engine::LinuxX86_64, "guests/x86/hx")]).has("42"),
            // REGRESSION GUARD: a static -no-pie x86_64 Go binary exercising the runtime scheduler + GC.
            // The non-PIE Go path rebases moduledata code PCs (text/minpc/maxpc) HIGH (elf.c go_rebase_nonpie)
            // so findfunc resolves the biased return PCs; but a rip-relative `LEAQ funcsym(SB)` materializes a
            // CODE address that findfunc must ALSO see HIGH. The whole-image lea->low rewrite over-applied to
            // these code leas -> findmoduledatap(low pc) failed -> zero funcInfo -> pctab[0:] empty -> step()
            // "index out of range" in runtime.init (asyncPreempt funcMaxSPDelta). Fix: for a Go image the
            // lea->low rewrite is confined to the type section (translate/x86_64/translate/mov.c), so code
            // pointers stay HIGH while type/data pointers stay LOW. Golden totals cross-checked byte-exact vs
            // native aarch64 Go (qemu-x86_64 cannot oracle Go GC: its lfstack pointer-packing breaks at high
            // heap addresses). go_goro exercises the goroutine scheduler + channels; go_heapgc adds heavy GC.
            fixture(
                "go-static-goro",
                &[(Engine::LinuxX86_64, "guests/x86/go_goro_x86")],
            )
            .has("goro tot= 16119975488"),
            fixture(
                "go-static-heapgc",
                &[(Engine::LinuxX86_64, "guests/x86/go_heapgc_x86")],
            )
            .has("OK heapgc total= 9922162"),
            // CPUID feature-flag completeness. Executes real CPUID and asserts every feature dd's
            // translator implements is ADVERTISED (SSE2/SSE4.2/POPCNT/AES/PCLMUL/BMI/SHA/ERMS/FSRM/NX/RDTSCP/LM
            // + the GenuineIntel vendor + "dd JIT x86-64 processor" brand) while AVX/AVX2/AVX512/FMA/XSAVE stay
            // OFF (dd can't translate VEX/EVEX -> advertising them would crash guests). Self-check verdict,
            // golden on dd (qemu advertises its own limited model, so exact bits can't be oracle-diffed).
            src("cpuid-features", "cpuid_features.c")
                .only(&[Engine::LinuxX86_64])
                .out("cpuid ok=1\n"),
            // RFLAGS.ID (bit 21) round-trip through pushfq/popfq (the 32-bit CPUID-availability probe).
            // Byte-exact vs the qemu-x86_64 oracle -- qemu models EFLAGS.ID correctly, so set=1/clr=0 must match.
            src("rflags-id", "rflags_id.c")
                .only(&[Engine::LinuxX86_64])
                .oracle(),
        ],
    )
}
