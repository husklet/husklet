//! Regression guards for shipped correctness bugs.

use crate::{fixture, group, port, src, src_nopie, Engine, Group};

/// Regression guards for shipped correctness bugs. Portable, golden-checked.
/// - lseek/offset: — apt-get update BADSIG. gpg's keyring_get_keyblock lseek(fd,found.offset,SEEK_SET)s
///   to the matched keyblock then read()s it; a stale/miswired seek served the read from offset 0, so the
///   FIRST key was re-read -> "BADSIG <Ubuntu Archive key>". Root cause: the overlay open path tagged a
///   REGULAR file as a directory stream (g_ovldir), so lseek(SEEK_SET) on it was redirected to a directory
///   rewind and never seeked the host fd. lseek_read/offset_track assert seek-then-read coherence.
/// - sha512/ccmp/lse: crypto + comparison + atomic primitives exercised during the investigation.
pub(super) fn regress() -> Group {
    group("regress", vec![
        port("lseek-read", "lseek_read.c").has("lseek-read OK"),   // seek-then-read coherence (bare)
        // REPRODUCER: same test under an OVERLAY (rootfs injected as its own lower) so the overlay
        // open path runs -- this is the configuration that tagged a regular file as a directory stream and
        // broke lseek(SEEK_SET). Fails pre-fix, passes post-fix. Linux engines only (darwin has no overlay).
        port("lseek-read-overlay", "lseek_read.c").rootfs("alpine").overlay()
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64]).has("lseek-read OK"),
        port("offset-track", "offset_track.c").has("offset-track OK"), // off_t/position bookkeeping + re-seek
        port("sha512-kat", "sha512_kat.c").has("135000 : be56780ee49bdf84968811e70c492d018b91274b0c94b5d2196545ceeacc43ed4b45415ce5a51a3f68608d3f232bba4f279230fc95319934f6ce9ec52e711cf8"),
        port("ccmp-chain", "ccmp_test.c").has("ccmp OK"),          // conditional-compare/branch chains
        // sched_getaffinity(tid) for a NON-main guest thread must not spuriously return ESRCH. glibc's
        // pthread_getattr_np (HotSpot's os::current_stack_region on EVERY JVM thread bring-up, also Go)
        // calls sched_getaffinity(pd->tid) first; dd validated that pid with host kill(guest_tid,0) -> ESRCH
        // (a guest tid is a dd-internal id, not a host pid) -> pthread_getattr_np returned 3 -> `java -version`
        // aborted "pthread_getattr_np failed with error = 3". Fix resolves guest tids via the live-thread
        // registry. Pre-fix: tid=0 wrap=0 getattr=0; post-fix matches native. Linux-only (no pthread_getattr_np
        // on macOS libc). Shared proc.c fix -> both arches.
        port("getaffinity-tid", "getaffinity_tid.c").only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
            .out("getaffinity-tid tid=1 self=1 wrap=1 getattr=1\n"),
        // LDAPR/LDAPRH/LDAPRB (Load-Acquire RCpc) on a NON-PIE image's low absolute data. The RCpc
        // load aliases the LSE atomic-RMW encoding box, so both the bias-fold (emit_fold_mem) and the
        // SIGSEGV fallback (nonpie_fixup) must serve it; nonpie_fixup formerly matched it as an atomic
        // (opc==4/o3==1) and returned 0 → a hard SIGSEGV. Diffed byte-exact vs the native aarch64 oracle.
        src_nopie("ldapr-nonpie", "nonpie_ldapr.c").oracle()
            .only(&[Engine::LinuxAarch64]), // LDAPR is an aarch64 RCpc opcode; no x86 analogue
        // Same guest with the bias-fold DISABLED (NOGUESTFOLD) so every LDAPR* on the low image faults into
        // nonpie_fixup — the exact path that formerly declined LDAPR (opc==4/o3==1 → return 0 → SIGSEGV).
        // Byte-exact vs native proves the fixup now serves the acquire load correctly.
        src_nopie("ldapr-nonpie-fixup", "nonpie_ldapr.c").env("NOGUESTFOLD", "1").oracle()
            .only(&[Engine::LinuxAarch64]),
        // aarch64 PAIR atomics (LDXP/STXP exclusive pair + CASP compare-and-swap pair) on a NON-PIE image's
        // low absolute .data. nonpie_fixup only served the single-register exclusive/CAS forms; the pair forms
        // fell through -> return 0 -> the low-address fault re-raised on the SAME instruction forever (hang).
        // Now emulated as a software 128-bit LL/SC + 128-bit CAS; byte-exact vs native.
        src_nopie("pairatomics-nonpie", "nonpie_pairatomics.c").oracle()
            .only(&[Engine::LinuxAarch64]),
        // Same guest with the bias-fold DISABLED so every LDXP/STXP/CASP faults into nonpie_fixup — the path
        // that formerly hung on the pair forms.
        src_nopie("pairatomics-nonpie-fixup", "nonpie_pairatomics.c").env("NOGUESTFOLD", "1").oracle()
            .only(&[Engine::LinuxAarch64]),
        // REGRESSION GUARD: an externally-linked / cgo (runtime.iscgo==1) aarch64 Go binary that forces
        // heavy goroutine stack growth + GC (64 goroutines, morestack copies, runtime.GC). Go async-preempts
        // running goroutines with SIGURG; dd's delivery of SIGURG into a preempted cgo thread (Go's
        // cgoSigtramp) corrupted a stack return address via a signal-frame/SP overlap -> SIGSEGV/SIGBUS mid-run
        // (proven: this fixture, influxd, and victoria-metrics all crashed). The interim fix auto-suppresses
        // SIGURG for exactly the iscgo aarch64 Go class (os/linux/elf.c detects it from the Go build-info
        // CGO_ENABLED setting; os/linux/signal.c drops the delivery) -- equivalent to GODEBUG=asyncpreemptoff=1,
        // so cooperative preemption keeps the program correct. Without the fix this crashes (rc=139/138); with
        // it, it completes. Prebuilt aarch64-only (no local Go cross-compiler); Go GC can't be qemu-oracled, so
        // the total is a GOLDEN cross-checked byte-exact vs native arm64 Go. Source: guests/arm/go_cgo_stackgrow.go
        // built `CGO_ENABLED=1 go build -ldflags='-linkmode external -extldflags -static'`.
        fixture("go-cgo-sigurg", &[(Engine::LinuxAarch64, "guests/arm/go_cgo_stackgrow_arm")])
            .has("OK stackgrow total= 2016"),
        // STACK-OVERFLOW GUARD: a guest that recurses off the bottom of its stack must hit the
        // PROT_NONE guard gap dd now places immediately below every guest stack -> a deliverable SIGSEGV
        // (like Linux's stack guard gap), NOT a silent write into the adjacent 64MB RX code cache (the
        // clickhouse corruption). The guest first proves the usable stack is intact (a bounded-but-deep
        // recursion prints "deep ok"), then overruns and dies of SIGSEGV. Byte-exact vs native arm64 /
        // qemu-x86_64: identical surviving stdout and identical signal-death. Pre-fix the x86 engine's lazy
        // stack-grow silently swallowed the overflow into the cache (wild crash / hang) -> mismatch.
        src("stackoverflow", "stackoverflow.c").oracle()
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64]),
        // item 3: a guest that installs its OWN SIGSEGV handler on an alternate signal stack (glibc's
        // stack-overflow detection / a JIT guard-page trap) must, on overflow, get that handler invoked with
        // signo SIGSEGV and a non-NULL si_addr in the guard region -- delivered on the altstack because the
        // main stack is exhausted (requires dd's per-thread host altstack + host-SIGBUS->SIGSEGV mapping).
        // Byte-exact "caught SIGSEGV addr=1" + exit 42 vs native arm64 / qemu-x86_64.
        src("stackoverflow-catch", "stackoverflow_catch.c").oracle()
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64]),
        // a NON-PIE x86_64 glibc ET_EXEC that dereferences a baked LOW absolute pointer through
        // the two C-emulated vector paths -- do_avx (VEX `vmovdqu ymm,[reg]`) and do_sse3b (legacy SSSE3
        // `pabsb [reg],xmm`). Both funnel every guest-memory operand through avx_ea(), which formerly
        // returned the low link address 1:1 instead of folding +g_nonpie_bias like the JIT-emitted path
        // (ea_bias17). The base-register operand then hit the UNMAPPED low vaddr -> SIGSEGV (exactly node
        // --version's V8 init crash). Fixed in translate/x86_64/avx.c. Byte-exact vs the qemu-x86_64 oracle.
        src_nopie("nonpie-vec", "nonpie_vec.c").oracle()
            .only(&[Engine::LinuxX86_64]), // AVX/SSSE3 are x86 opcodes; no aarch64 analogue
        // Same guest with the EMIT-time bias-fold disabled (NOGUESTFOLD): the JIT-emitted loads now fault
        // into nonpie_fixup while the C vector path (avx_ea) still folds directly -- proving the C-side fold
        // is independent of the emit-time kill-switch and both routes serve the low image byte-exact.
        src_nopie("nonpie-vec-fixup", "nonpie_vec.c").env("NOGUESTFOLD", "1").oracle()
            .only(&[Engine::LinuxX86_64]),
        // rep cmps/scas whose string operand is a LOW.rodata pointer in a biased non-PIE image
        // (node:20 x86 `node --version` emits `mov edi,<flagstr>; rep cmpsb`). The do_repstr C helper
        // dereferenced rsi/rdi 1:1 (unlike rep movs/stos, which rebase via repstr_g2h) -> SIGSEGV on the
        // unmapped low vaddr. Fixed in translate/x86_64/x86_ops.c. Byte-exact vs the qemu-x86_64 oracle.
        src_nopie("repcmps-nonpie", "repcmps_nopie.c").oracle()
            .only(&[Engine::LinuxX86_64]), // rep cmps/scas are x86 opcodes; no aarch64 analogue
        // aarch64 PC-relative literal-load family. A literal load reads its constant at an address
        // relative to the GUEST PC; dd places the block at a DIFFERENT host address, so each such load must
        // be rewritten to materialize the guest-absolute literal address. LDRSW (literal) (opc=10, V=0, top
        // byte 0x98 — the sign-extending word load compilers emit for switch/jump tables) was MISSING from
        // that rewrite (it only "worked" when the host arena happened to place the literal in reach), and
        // PRFM (literal) (0xD8) fell through to a verbatim host-PC-relative emit. This guest drives the WHOLE
        // family (0x18/0x58 LDR-lit W/X, 0x98 LDRSW-lit, 0x1C/0x5C/0x9C LDR-lit SIMD S/D/Q, 0xD8 PRFM-lit);
        // byte-exact vs the native aarch64 oracle. Literal loads are aarch64-only (no x86 analogue).
        src("ldrsw-literal", "ldrsw_literal.c").oracle()
            .only(&[Engine::LinuxAarch64]),
        // Same guest under the persistent translation cache (DDJIT_PCACHE): the matrix alternates cold(save)
        // and warm(load) on a fixed dir, so successive runs exercise the WARM path — exactly where the old
        // accidental-host-placement bug bit (a restored arena at a different base would resolve the literal
        // to the wrong host-relative bytes). The literal must resolve to the identical guest value cold or
        // warm; diffed byte-exact vs native either way.
        src("ldrsw-literal-pcache", "ldrsw_literal.c")
            .env("DDJIT_PCACHE", "1").env("DDJIT_PCACHE_DIR", "/tmp/ddjit-pcache-ldrsw")
            .oracle().only(&[Engine::LinuxAarch64]),
        // V8's embedded-builtins CODE base (symbol v8_Default_embedded_blob_code_) is a baked LOW.text
        // address loaded via `mov r,imm`; the builtins execute at the HIGH mapping, so V8's
        // InnerPointerToCodeCache range check (LOW base vs a HIGH stack return address) missed -> V8_Fatal
        // maybe_code.has_value() (node:20 `new Error().stack` / mongosh). The loader records that symbol and
        // the frontend rebases its mov-imm materialization HIGH (translate/x86_64), so the code range matches
        // execution WITHOUT touching return addresses (Go's HIGH-PC stack-walk stays intact -- see go-static).
        // This guard reproduces the base-vs-return-address same-half invariant. Oracle-diffed; NOV8BLOB=1 = off.
        src_nopie("nonpie-v8blob", "nonpie_v8blob.c").oracle()
            .only(&[Engine::LinuxX86_64]),
    ])
}
