//! Heavy / soak workloads, real toolchains (gcc), real software, and busybox applets.

use crate::support::{group, in_rootfs, port, src, Engine, Group};

use super::sh;

/// Long-running / heavy-footprint workloads — a sustained compute loop and a large sparse mmap (both
/// portable), plus a postgres-shaped networked DB service. The "is it actually stable under load" tier.
pub(super) fn heavy() -> Group {
    group(
        "heavy",
        vec![
            port("busyloop", "busyloop.c").out("busyloop acc=14881893564601462335\n"), // 300M-iter mixing loop
            // dispatch shapes, golden-checked: a 128-target megamorphic computed-goto VM (one wrong
            // IBTC/ctx prediction corrupts the checksum) + monomorphic deep recursion (real call/ret traffic).
            src("ibtc-dispatch", "ibtc_dispatch.c")
                .out("ibtc vm=10240120795314104034 rec=2178309 chk=12619423276023875997\n"),
            port("bigmem", "bigmem.c").out("bigmem pages=131072 sum=16711680\n"), // mmap 512 MiB, fault pages
            // #104: large heap array (2M u64, 16 MiB, idx > 2^20). Differential vs qemu: catches a
            // 32-bit index/offset truncation or large-copy length truncation in the x86->ARM64 lowering.
            src("bigarr", "bigarr.c")
                .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
                .oracle(),
            // fork-per-connection TCP server backed by real SQLite (WAL) + a 50-connection client (links
            // libsqlite3 -> Linux/aarch64 only), diffed against a native run.
            src("dbserver", "dbserver.c")
                .only(&[Engine::LinuxAarch64])
                .oracle(),
        ],
    )
}

/// SOAK / endurance — workloads that run for a sustained stretch and only fail through the JIT's
/// long-run machinery: code-cache recycling, block-chaining/IBTC drift, self-modifying-code
/// re-translation, and resource churn (threads/forks/heap) accumulating over thousands of cycles.
/// These catch bugs a short test never reaches. Deterministic -> golden (portable) / oracle (smc).
pub(super) fn soak() -> Group {
    group(
        "soak",
        vec![
            port("codecache", "soak_codecache.c").out("soak codecache acc=5966323930328914303\n"), // 256 blocks, 120M iters
            port("indirect", "soak_indirect.c").out("soak indirect acc=4633281659943884454\n"), // 64-target IBTC, 80M iters
            port("threadchurn", "soak_threadchurn.c").out("soak threadchurn total=14000000\n"), // 4000 short threads
            port("forkchurn", "soak_forkchurn.c").out("soak forkchurn reaped=3000 acc=151500\n"), // 3000 fork/wait
            port("allocchurn", "soak_allocchurn.c").out("soak allocchurn sum=1529986411\n"), // 6M malloc/free
            // self-modifying code: patch+flush+call an RWX page 200k times -> re-translation churn. aarch64
            // machine code, so Linux/aarch64 only (the real JIT path); diffed vs native. xfail: mmap(RWX)
            // is EPERM under macOS W^X (no MAP_JIT) -> guest-JIT runtimes can't get exec pages (see PLAN.md).
            src("smc", "soak_smc.c")
                .only(&[Engine::LinuxAarch64])
                .oracle(),
            // MULTITHREADED self-modifying code sharing code pages (the BeamAsm/Erlang crash class) --
            // 8 threads append+rewrite `movz;ret` slots off a shared bump pointer so translations from
            // different threads collide on the same pages while `ic ivau` fires. Regressed as a
            // non-deterministic SIGSEGV/SIGBUS (unlocked page-granular SMC drop racing peers). aarch64 only.
            src("smcthreads", "smc_threads.c")
                .only(&[Engine::LinuxAarch64])
                .oracle(),
            // SMC self-flush LIVELOCK guard (the chromium/V8 startup wall): a hot loop re-flushes an
            // already-translated, UNCHANGED code line (its own executing block) while a large working set
            // is live. The old wholesale-drop SMC hook re-translated the whole working set on EVERY such
            // benign flush -> `translate_block` spins forever (25s timeout -> FAIL). The content-gated
            // drop skips a flush whose bytes did not change -> the working set is translated once -> fast.
            // Bytes never change, so the checksum is identical either way; diffed vs a native run.
            src("smcselfflush", "smc_selfflush.c")
                .only(&[Engine::LinuxAarch64])
                .oracle(),
            // H9 / #423: mprotect(PROT_EXEC) must ARM SMC re-translation. A mmap(RW)->write->mprotect(RX)
            // JIT toggle (the .NET/Wasm x86 pattern -- NOT an mmap(RWX) arena) rewrites its code page across
            // three rounds; a correct engine invalidates each stale translation and returns 111/222/333, a
            // broken one caches the first and returns 111/111/111. x86 is the exposed arch (coherent i-cache,
            // no `ic ivau` to hook -- the SMC write-fault is the only invalidation signal); aarch64 rides
            // along as a second witness. Portable inline machine code -> both Linux engines, golden-checked.
            port("smcmprotect", "smc_mprotect.c")
                .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
                .out("smc mprotect r1=111 r2=222 r3=333\n"),
            // Stale translation after VA REUSE: an executable VA is translated, then unmapped + re-mapped
            // (munmap+MAP_FIXED, then MAP_FIXED-in-place) with DIFFERENT code. The dispatcher keys cached host
            // code by guest PC, so without invalidation on unmap/MAP_FIXED it re-runs the OLD translation
            // (111/111/111). x86 machine code -> LinuxX86_64 only; golden-checked.
            src("smcremapreuse", "smc_remap_reuse.c")
                .only(&[Engine::LinuxX86_64])
                .out("smc remap r1=111 r2=222 r3=333\n"),
            // Same class through mremap(MREMAP_FIXED): a translated VA is relocated and its freed source VA is
            // re-mapped with new code; the source VA's stale translation must be dropped (else second=11).
            src("smcmremap", "smc_mremap.c")
                .only(&[Engine::LinuxX86_64])
                .out("smc mremap first=11 moved=1 second=22\n"),
            // SMC protection-table overflow: after SMC_MAX 16 KB code pages are translated+protected, a further
            // page was left read-only but UNTRACKED (mprotect ran before the capacity check), so a later
            // rewrite of it faulted un-handled -> SIGSEGV/hang. Fills the table past the limit, then rewrites +
            // re-runs a LATE page: a correct engine returns the patched value. x86 machine code, golden.
            src("smctableoverflow", "smc_table_overflow.c")
                .only(&[Engine::LinuxX86_64])
                .out("smc overflow patched=4242\n"),
        ],
    )
}

/// COMPILE — the worst-case technical workload an engineer runs: a real GCC-14 toolchain
/// (gcc -> cc1/cc1plus -> as -> collect2/ld, a deep fork/exec/pipe pipeline over hundreds of MB of
/// headers/libs) compiling C and C++ *inside the container*, then running the result and checking its
/// output. If the JIT can host a compiler building+running correct code, it can host almost anything.
/// Sources are embedded base64 (self-contained, no host source needed). Linux/aarch64 (the gcc image).
pub(super) fn compile() -> Group {
    let gcc = |name, sh| in_rootfs(name, "gcc-bundle", &["/bin/sh", "-c", sh]);
    group("compile", vec![
        // the staged /hello.c — gcc -> cc1 -> as -> ld -> run. Still segfaults under the aarch64 JIT
        // (large dynamically-linked driver; no missing-syscall/UNIMPL diagnostic -> codegen/runtime bug).
        // Marked xfail so the gap is tracked (XPASS will fire the moment the engine fixes it).
        gcc("hello", "cd /tmp && gcc-14 -O2 -o h /hello.c && ./h && rm -f h").has("compiled by gcc")
            .xfail(&[Engine::LinuxAarch64]),
        // a prime sieve (pure integer -> optimizer-independent output); proves compiled code is correct.
        gcc("c-primes", "cd /tmp && echo I2luY2x1ZGUgPHN0ZGlvLmg+CmludCBtYWluKHZvaWQpe2ludCBjPTA7Zm9yKGludCBuPTI7bjwxMDAwMDA7bisrKXtpbnQgcD0xO2ZvcihpbnQgZD0yO2QqZDw9bjtkKyspaWYobiVkPT0wKXtwPTA7YnJlYWs7fWMrPXA7fXByaW50ZigicHJpbWVzPSVkXG4iLGMpO3JldHVybiAwO30K \
            | base64 -d > p.c && gcc-14 -O2 -o p p.c && ./p && rm -f p p.c").has("primes=9592"),
        // C++: g++ -> cc1plus -> libstdc++ (vector/sort/string) -- the heavy template+STL link path.
        gcc("cpp-stl", "cd /tmp && echo I2luY2x1ZGUgPHZlY3Rvcj4KI2luY2x1ZGUgPGFsZ29yaXRobT4KI2luY2x1ZGUgPHN0cmluZz4KI2luY2x1ZGUgPGNzdGRpbz4KaW50IG1haW4oKXsKICBzdGQ6OnZlY3Rvcjxsb25nPiB2OwogIGZvcihpbnQgaT0wO2k8MTAwMDAwO2krKykgdi5wdXNoX2JhY2soKGxvbmcpKChpKjI2NTQ0MzU3NjF1KSUxMDAwMDAzKSk7CiAgc3RkOjpzb3J0KHYuYmVnaW4oKSwgdi5lbmQoKSk7CiAgbG9uZyBzPTA7IGZvcihsb25nIHg6IHYpIHMrPXg7CiAgc3RkOjpzdHJpbmcgYT0iZGQiLCBiPSItY3BwIjsgYSs9YjsKICBwcmludGYoImNwcCBuPSV6dSBzdW09JWxkIG1lZD0lbGQgcz0lc1xuIiwgdi5zaXplKCksIHMsIHZbdi5zaXplKCkvMl0sIGEuY19zdHIoKSk7CiAgcmV0dXJuIDA7Cn0K \
            | base64 -d > p.cpp && g++-14 -O2 -o p p.cpp && ./p && rm -f p p.cpp").has("cpp n=100000 sum=50002557337 med=500032 s=dd-cpp"),
    ])
}

/// Real software inside a container — busybox applets doing networked + long-running + compression
/// work in the alpine rootfs. Exercises the container path (rootfs jail, private-loopback netns, fork/
/// exec of real binaries) the way an actual workload would. Linux/aarch64 (containers are Linux).
pub(super) fn containersw() -> Group {
    group("containersw", vec![
        // busybox nc over the container's private loopback: a listener writes what it receives to a
        // file, a client sends a line, then we read it back. Exercises the netns lo_* unix-socket path.
        sh("nc-loopback", "(nc -l -p 18080 > /tmp/srv.out &) ; sleep 1; echo hello-nc | nc 127.0.0.1 18080; \
            sleep 1; cat /tmp/srv.out; rm -f /tmp/srv.out").has("hello-nc"),
        // gzip/gunzip roundtrip through a pipe (real DEFLATE, fork of two applets).
        sh("gzip", "echo compress-me-12345 > /tmp/d; gzip -c /tmp/d | gunzip -c; rm -f /tmp/d")
            .out("compress-me-12345\n"),
        // tar a directory to stdout and untar it elsewhere (streamed archive, fork/exec + fs churn).
        sh("tar", "cd /tmp; rm -rf ta tb; mkdir ta tb; echo content-X > ta/f1; tar cf - ta | (cd tb; tar xf -); \
            cat tb/ta/f1; rm -rf ta tb").has("content-X"),
        // a long-running shell arithmetic loop (200k iterations of ash $((...)) -- a multi-second guest).
        sh("longshell", "i=0; s=0; while [ $i -lt 200000 ]; do s=$((s+i)); i=$((i+1)); done; echo sum=$s")
            .has("sum=19999900000"),
    ])
}

/// Real software — the actual upstream engines, static-linked, doing real work. The acid test that the
/// runtime handles production code (file I/O, mmap, fsync, locking, libc breadth), not just microbench.
pub(super) fn realsw() -> Group {
    group("realsw", vec![
        // SQLite 3: WAL, a 5000-row transaction, then an aggregate query. Diffed against a native run.
        src("sqlite", "sqlite.c").arg("/tmp/hl_sqlite_test.db").only(&[Engine::LinuxAarch64]).oracle(),
        // Perl 5 (the real Ubuntu interpreter): a prime sieve up to 10k -- heavy interpreter loop +
        // dynamic dispatch. 1229 primes, last is 9973.
        in_rootfs("perl-sieve", "ubuntu", &["/usr/bin/perl", "-e",
            "my @p; for my $n (2..10000){ my $pr=1; for my $d (2..int(sqrt($n))){ $pr=0,last if $n%$d==0 } \
             push @p,$n if $pr } print \"primes=\".scalar(@p).\" last=$p[-1]\\n\""])
            .has("primes=1229 last=9973"),
        // Intensive container-fs IO: create/write/read 200 files in a tight shell loop (open/write/close/
        // readdir/unlink churn through the rootfs jail).
        in_rootfs("io-churn", "ubuntu", &["/bin/sh", "-c",
            "cd /tmp && rm -rf io && mkdir io && cd io && i=0; while [ $i -lt 200 ]; do echo data-$i payload > f$i; \
             i=$((i+1)); done; cat f* | wc -l; cd /; rm -rf /tmp/io"])
            .has("200"),
    ])
}

/// busybox applets inside the alpine rootfs — golden output (aarch64 image).
pub(super) fn busybox() -> Group {
    group(
        "busybox",
        vec![
            sh("echo", "echo hello world").out("hello world\n"),
            sh("printf", "printf '%d-%s\\n' 42 hi").out("42-hi\n"),
            sh("expr", "expr 6 \\* 7").out("42\n"),
            sh("seq", "seq 1 5 | tr '\\n' ' '").out("1 2 3 4 5 "),
            sh("wc", "printf 'a\\nb\\nc\\n' | wc -l").has("3"),
            sh("tr", "echo hello | tr a-z A-Z").out("HELLO\n"),
            sh("cut", "echo a:b:c | cut -d: -f2").out("b\n"),
            sh("head", "seq 1 100 | head -3 | tr '\\n' ' '").out("1 2 3 "),
            sh("tail", "seq 1 100 | tail -2 | tr '\\n' ' '").out("99 100 "),
            sh("rev", "echo abc | rev").out("cba\n"),
            sh("sort", "printf 'c\\nb\\na\\n' | sort | tr '\\n' ' '").out("a b c "),
            sh("uniq", "printf 'a\\na\\nb\\n' | uniq | tr '\\n' ' '").out("a b "),
            sh("grep", "printf 'foo\\nbar\\n' | grep bar").out("bar\n"),
            sh("sed", "echo hello | sed s/l/L/g").out("heLLo\n"),
            sh("awk", "echo '3 4' | awk '{print $1+$2}'").out("7\n"),
            sh("basename", "basename /a/b/c.txt").out("c.txt\n"),
            sh("dirname", "dirname /a/b/c").out("/a/b\n"),
            sh("base64", "printf abc | base64").out("YWJj\n"),
            sh("md5", "printf abc | md5sum").has("900150983cd24fb0d6963f7d28e17f72"),
            sh("find", "find /etc -name hostname 2>/dev/null | head -1").has("/etc/hostname"),
        ],
    )
}
