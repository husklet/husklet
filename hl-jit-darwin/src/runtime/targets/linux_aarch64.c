// dd/runtime -- ddjit_aarch64: the aarch64-Linux-guest JIT runner (unity translation unit).
//
// A same-ISA aarch64->aarch64 JIT services the guest's Linux syscalls in userspace (no VM). This TU
// pulls in the engine (jit/), the aarch64 guest frontend (frontend/aarch64/), the Linux personality +
// container layer (os/linux/), and defines dd_run() (the Rust binding's entry) + main(). The x86-64
// guest reuses os/linux/ + jit/ with frontend/x86_64/ (see ddjit_x86_64.c).
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <pthread.h>
#include <errno.h>
#include <time.h>
#include <sys/time.h>
#include <sys/uio.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <sys/un.h>
#include <poll.h>
#include <signal.h>
#include <dirent.h>
#include <libkern/OSCacheControl.h>
#include <mach/mach.h>
#include <mach/mach_vm.h> // mach_vm_remap/protect for the dual-mapped RW/RX code cache
#define DD_HAS_MACH_EXC 1 // service.c gates its CRASHDBG fork-child Mach re-arm on this
#include <dlfcn.h>
#include <sys/event.h>
#include <termios.h>
#include <sys/ioctl.h>
#include <sys/resource.h>
#include <stdatomic.h>

#include "../include/cpu_aarch64.h"
#include "../translate/aarch64/abi.h"       // the cpu interface os/linux/ is written against
#include "../translate/aarch64/fill_stat.c" // the per-arch struct-stat layout os/linux/ fills
// Byte size of the guest `struct stat` fill_stat.c writes -- the shared stat syscalls (os/linux/syscall/
// fs.c cases 79/80) validate exactly this many guest bytes before filling the buffer (EFAULT guard).
#define GUEST_LINUX_STAT_BYTES 128

// container/ns config state + parsers (early globals)
#include "../os/linux/container/state.c"
// code cache + block map + chaining
#include "../engine/cache.c"
// host ARM64 assembler (emit32 + e_* encoders) -- the lowest layer
#include "../host/arm64/asm.c"
// persistent cross-process translated-code cache (recorded emitters used by stubs.c/translate.c;
// load/save/relocate). MUST precede stubs.c + translate.c (they call the recorded emitters).
#include "../translate/aarch64/pcache.c"
// engine block-ABI stubs: prologue/spill + IBTC/IC + exit/chain trampolines (built on the assembler above)
#include "../engine/stubs.c"
// transliterate + mangle + §B + LSE + depth-gate
#include "../translate/aarch64/translate.c"
// clone/futex/threads (declares run_guest)
#include "../os/linux/thread.c"
// signal delivery
#include "../os/linux/signal.c"
#include "../translate/aarch64/sigframe.c" // per-arch rt_sigframe build/restore (uses signal.c state)
// path jail + overlay + /proc synth
#include "../os/linux/container/vfs.c"
// termios + NET-ns loopback
#include "../os/linux/container/netns.c"
// ELF fwd-decls + FS-metadata cache
#include "../os/linux/fscache.c"
// the syscall layer (service())
#include "../os/linux/syscall/dispatch.c"
// untrusted-guest isolation: SPSC ring + sentry split (g_untrusted; OFF by default)
#include "../os/linux/sentry.c"
// native checkpoint/restore control seam: the shared dispatcher (engine/dispatch.c) polls G_CKPT_POLL at
// its per-block safepoint. Declared here (before the include) so only the aarch64 TU wires it; ckpt_poll is
// DEFINED in os/linux/checkpoint.c below (a forward-declared static call is legal).
static void ckpt_poll(struct cpu *c);
#define G_CKPT_POLL(c) ckpt_poll(c)
// checkpoint.c's restore driver (included below) rebuilds the container from these, defined later in this TU.
static void container_init(const char *rootfs);
static int engine_global_init(void);
// host trampoline + run_guest
#include "../engine/dispatch.c"
// ELF loader + initial stack
#include "../os/linux/elf.c"
// native checkpoint/restore (multi-process tree): dump/restore guest RAM + cpu + path-backed fds + pty
#include "../os/linux/checkpoint.c"
// `--configfd` launch bridge: read the serialized ddjit_config from the fd, re-hydrate DD_*/DDJIT_* env,
// and dispatch to this TU's dd_run() (forward-declared inside; defined below).
#include "../os/ddjit_configfd.c"

// ---- library entry (Rust binding) + main() ----
// ---------------- library entry (Rust bindings call this) ----------------
// Loads `argv[0]` (a guest aarch64 ELF, path resolved inside `rootfs` if given),
// runs it to completion, and returns the guest's exit code. argv is the guest
// argv (program + args). Single-shot per process: the daemon forks a child per
// container and calls this once. Declared in jit.h.
static void diag_hx(char *b, uint64_t v) {
    for (int i = 0; i < 16; i++) {
        int d = (v >> ((15 - i) * 4)) & 0xf;
        b[i] = d < 10 ? '0' + d : 'a' + d - 10;
    }
}

static void diag_hx8(char *b, uint32_t v);

static int diag_reg(char *b, int bp, int r, uint64_t v) {
    b[bp++] = ' ';
    b[bp++] = 'x';
    if (r >= 10) b[bp++] = (char)('0' + r / 10);
    b[bp++] = (char)('0' + r % 10);
    b[bp++] = '=';
    b[bp++] = '0';
    b[bp++] = 'x';
    diag_hx(b + bp, v);
    return bp + 16;
}

// async-signal-safe (write only)
static void diag_crash(int s, siginfo_t *si, void *uc) {
    // mirror the normal-path nonpie_guard so CRASHDBG does not false-report faults that path
    // resolves. A non-PIE ET_EXEC's absolute DATA ref into its low link range is served at +bias
    // (nonpie_fixup); a fault a guest handler owns (e.g. gcc's SIGSEGV handler) is delivered to the guest
    // (deliver_guest_fault). Only a genuinely unresolved fault falls through to the [CRASH] report below --
    // that is the whole point of CRASHDBG. This POSIX handler covers forked children (whose inherited Mach
    // exception port does not survive fork), so it must resolve the same faults the Mach path does.
    if (nonpie_fixup(si, uc)) return;
    if (deliver_guest_fault(s, si, uc)) return;
    struct cpu *c = (struct cpu *)pthread_getspecific(g_cpu_key);
    ucontext_t *u = (ucontext_t *)uc;
    uint64_t hpc = u ? (uint64_t)u->uc_mcontext->__ss.__pc : 0;
    uint64_t hx0 = u ? (uint64_t)u->uc_mcontext->__ss.__x[0] : 0;
    uint64_t hx1 = u ? (uint64_t)u->uc_mcontext->__ss.__x[1] : 0;
    uint64_t hx9 = u ? (uint64_t)u->uc_mcontext->__ss.__x[9] : 0;
    uint64_t hx10 = u ? (uint64_t)u->uc_mcontext->__ss.__x[10] : 0;
    uint64_t hx16 = u ? (uint64_t)u->uc_mcontext->__ss.__x[16] : 0;
    uint64_t hx17 = u ? (uint64_t)u->uc_mcontext->__ss.__x[17] : 0;
    uint64_t hx30 = u ? (uint64_t)u->uc_mcontext->__ss.__x[30] : 0;
    char b[1600];
    for (int i = 0; i < 1600; i++)
        b[i] = ' ';
    memcpy(b, "[CRASH] sig=X fault=0x", 22);
    b[11] = '0' + (s % 10);
    diag_hx(b + 22, (uint64_t)si->si_addr);
    memcpy(b + 38, " pc=0x", 6);
    diag_hx(b + 44, c ? c->pc : 0);
    memcpy(b + 60, " hpc=0x", 7);
    diag_hx(b + 67, hpc);
    memcpy(b + 83, " tid=0x", 7);
    diag_hx(b + 90, (uint64_t)(c ? c->ctid : 0));
    int bp = 106;
    memcpy(b + bp, " x16=0x", 7);
    bp += 7;
    diag_hx(b + bp, hx16);
    bp += 16;
    memcpy(b + bp, " x17=0x", 7);
    bp += 7;
    diag_hx(b + bp, hx17);
    bp += 16;
    memcpy(b + bp, " x30=0x", 7);
    bp += 7;
    diag_hx(b + bp, hx30);
    bp += 16;
    memcpy(b + bp, " x0=0x", 6);
    bp += 6;
    diag_hx(b + bp, hx0);
    bp += 16;
    memcpy(b + bp, " x1=0x", 6);
    bp += 6;
    diag_hx(b + bp, hx1);
    bp += 16;
    memcpy(b + bp, " x9=0x", 6);
    bp += 6;
    diag_hx(b + bp, hx9);
    bp += 16;
    memcpy(b + bp, " x10=0x", 7);
    bp += 7;
    diag_hx(b + bp, hx10);
    bp += 16;
    if (u) {
        for (int r = 2; r <= 8; r++) bp = diag_reg(b, bp, r, (uint64_t)u->uc_mcontext->__ss.__x[r]);
        for (int r = 11; r <= 15; r++) bp = diag_reg(b, bp, r, (uint64_t)u->uc_mcontext->__ss.__x[r]);
        for (int r = 18; r <= 27; r++) bp = diag_reg(b, bp, r, (uint64_t)u->uc_mcontext->__ss.__x[r]);
    }
    extern int jit_pc_in_retained_cache(uint64_t pc);
    memcpy(b + bp, " jit=0x", 7);
    bp += 7;
    diag_hx8(b + bp, jit_pc_in_retained_cache(hpc) ? 1 : 0);
    bp += 8;
    extern int jit_hostpc_alias_kind(uint64_t hpc);
    memcpy(b + bp, " alias=0x", 9);
    bp += 9;
    diag_hx8(b + bp, jit_hostpc_alias_kind(hpc));
    bp += 8;
    extern void jit_cache_diag(uint64_t *gen, uint64_t *flushes, uint32_t *retired, uint32_t *freed);
    uint64_t cgen = 0, stwf = 0;
    uint32_t nret = 0, nfree = 0;
    jit_cache_diag(&cgen, &stwf, &nret, &nfree);
    memcpy(b + bp, " cgen=0x", 8);
    bp += 8;
    diag_hx(b + bp, cgen);
    bp += 16;
    memcpy(b + bp, " stwf=0x", 8);
    bp += 8;
    diag_hx(b + bp, stwf);
    bp += 16;
    memcpy(b + bp, " ret=0x", 7);
    bp += 7;
    diag_hx8(b + bp, nret);
    bp += 8;
    memcpy(b + bp, " freed=0x", 9);
    bp += 9;
    diag_hx8(b + bp, nfree);
    bp += 8;
    extern int jit_hostpc_lookup(uint64_t hpc, uint64_t *gpc, uint64_t *off, uint32_t *insn);
    uint64_t hgpc = 0, hoff = 0;
    uint32_t hinsn = 0;
    if (jit_hostpc_lookup(hpc, &hgpc, &hoff, &hinsn)) {
        memcpy(b + bp, " hblk=0x", 8);
        bp += 8;
        diag_hx(b + bp, hgpc);
        bp += 16;
        memcpy(b + bp, " hoff=0x", 8);
        bp += 8;
        diag_hx(b + bp, hoff);
        bp += 16;
        memcpy(b + bp, " hinsn=0x", 9);
        bp += 9;
        diag_hx8(b + bp, hinsn);
        bp += 8;
    }
    b[bp] = '\n';
    if (write(2, b, bp + 1) < 0) {}
    _exit(139);
}

static void diag_hx8(char *b, uint32_t v) {
    for (int i = 0; i < 8; i++) {
        int d = (v >> ((7 - i) * 4)) & 0xf;
        b[i] = d < 10 ? '0' + d : 'a' + d - 10;
    }
}

static mach_port_t g_exc_port;
// MiG lays exception messages out with 4-byte packing (see <mach/exc.h> `#pragma pack(push, 4)`),
// so the 64-bit `code[]` array immediately follows `codeCnt` at a 4-byte-aligned offset with NO padding.
// Without the pack, the compiler 8-byte-aligns `int64_t code[]` and inserts 4 bytes of padding, so `code[0]`
// / `code[1]` read 4 bytes past the kernel's data -- the fault address (code[1]) then comes back as 0 while
// the real address bleeds into code[0]. Match MiG's packing so the fault address is read correctly.
#pragma pack(push, 4)

typedef struct {
    mach_msg_header_t Head;
    mach_msg_body_t body;
    mach_msg_port_descriptor_t thread, task;
    NDR_record_t NDR;
    exception_type_t exception;
    mach_msg_type_number_t codeCnt;
    int64_t code[2];
    char pad[64];
} exc_msg_t;

#pragma pack(pop)

// Reply for a Mach exception (EXCEPTION_DEFAULT + MACH_EXCEPTION_CODES). RetCode=KERN_SUCCESS resumes the
// (possibly state-modified) thread; the reply msgh_id is the request id + 100 (MiG convention).
typedef struct {
    mach_msg_header_t Head;
    NDR_record_t NDR;
    kern_return_t RetCode;
} exc_reply_t;

// the CRASHDBG Mach port must resolve the SAME faults nonpie_guard resolves on the normal run
// path, instead of false-reporting them as crashes. nonpie_guard does nonpie_fixup() then, if that
// declines, deliver_guest_fault() (a fault a guest handler owns -- e.g. gcc registers a SIGSEGV handler);
// only an unresolved fault is a real crash. Mirror that here: rebuild the faulting thread's ARM/NEON state
// into an mcontext, run the same two resolvers, and (if either resolves it) write the updated state back so
// the KERN_SUCCESS reply resumes the thread correctly. Returns 1 if resolved (resume), 0 if a real crash.
static int mach_resolve_fault(mach_port_t thread, int hostsig, uint64_t fault, arm_thread_state64_t *ss) {
    _STRUCT_MCONTEXT64 mc;
    memset(&mc, 0, sizeof mc);
    mc.__ss = *ss;
    mach_msg_type_number_t nc = ARM_NEON_STATE64_COUNT;
    if (thread_get_state(thread, ARM_NEON_STATE64, (thread_state_t)&mc.__ns, &nc) != KERN_SUCCESS) return 0;
    ucontext_t uc;
    memset(&uc, 0, sizeof uc);
    uc.uc_mcontext = &mc;
    siginfo_t si;
    memset(&si, 0, sizeof si);
    si.si_addr = (void *)fault;
    // nonpie_fixup self-declines when the fault is not a non-PIE low-range access; deliver_guest_fault
    // self-declines (returns 0) when the guest has no handler or the host PC is outside the code cache
    // (a genuine engine fault) -- both then fall through to the [MACH] crash report below.
    // This runs on the dedicated exc_thread, NOT the faulting thread, so g_cpu_key TLS is NULL here --
    // pass the FAULTING thread's cpu explicitly. x28 is the engine's reserved CPUREG in translated code
    // (cpu_aarch64.h), so ss->__x[28] is the faulting thread's struct cpu whenever the fault is in the
    // code cache -- the only case deliver_guest_fault_hint dereferences it (it validates the host PC first).
    // Without this the exc_thread found no cpu and declined EVERY guest-handled fault -> a spurious [MACH].
    struct cpu *fcpu = (struct cpu *)ss->__x[28];
    if (!nonpie_fixup(&si, &uc) && !mach_async_fault_signal(fcpu, hostsig, &si) &&
        !deliver_guest_fault_hint(fcpu, hostsig, &si, &uc))
        return 0;
    thread_set_state(thread, ARM_THREAD_STATE64, (thread_state_t)&mc.__ss, ARM_THREAD_STATE64_COUNT);
    thread_set_state(thread, ARM_NEON_STATE64, (thread_state_t)&mc.__ns, ARM_NEON_STATE64_COUNT);
    return 1;
}

// catches faults on ALL threads (incl MAP_JIT workers)
static void *exc_thread(void *arg) {
    (void)arg;
    exc_msg_t msg;
    for (;;) {
        if (mach_msg(&msg.Head, MACH_RCV_MSG, 0, sizeof msg, g_exc_port, MACH_MSG_TIMEOUT_NONE, MACH_PORT_NULL) !=
            MACH_MSG_SUCCESS)
            continue;
        arm_thread_state64_t st;
        mach_msg_type_number_t cnt = ARM_THREAD_STATE64_COUNT;
        kern_return_t gs = thread_get_state(msg.thread.name, ARM_THREAD_STATE64, (thread_state_t)&st, &cnt);
        // resolve a fault the normal path would serve (non-PIE absolute data, or a guest-handled
        // fault) and resume the thread via a KERN_SUCCESS reply, matching nonpie_guard. EXC_BAD_ACCESS maps
        // to a guest SIGSEGV, EXC_BAD_INSTRUCTION to SIGILL; only an unresolved fault is a genuine crash.
        int hostsig = (msg.exception == EXC_BAD_INSTRUCTION) ? SIGILL
                      : (msg.exception == EXC_BREAKPOINT)    ? SIGTRAP // a guest `brk` (chromium IMMEDIATE_CRASH)
                                                             : SIGSEGV;
        if (gs == KERN_SUCCESS && mach_resolve_fault(msg.thread.name, hostsig, (uint64_t)msg.code[1], &st)) {
            exc_reply_t reply;
            memset(&reply, 0, sizeof reply);
            reply.Head.msgh_bits = MACH_MSGH_BITS(MACH_MSGH_BITS_REMOTE(msg.Head.msgh_bits), 0);
            reply.Head.msgh_size = sizeof reply;
            reply.Head.msgh_remote_port = msg.Head.msgh_remote_port;
            reply.Head.msgh_local_port = MACH_PORT_NULL;
            reply.Head.msgh_id = msg.Head.msgh_id + 100;
            reply.NDR = NDR_record;
            reply.RetCode = KERN_SUCCESS;
            mach_msg(&reply.Head, MACH_SEND_MSG, sizeof reply, 0, MACH_PORT_NULL, MACH_MSG_TIMEOUT_NONE,
                     MACH_PORT_NULL);
            continue;
        }
        char b[1600];
        for (int i = 0; i < 1600; i++)
            b[i] = ' ';
        memcpy(b, "[MACH] exc=0x", 13);
        diag_hx8(b + 13, msg.exception);
        memcpy(b + 21, " gs=0x", 6);
        diag_hx8(b + 27, gs);
        memcpy(b + 35, " fault=0x", 9);
        diag_hx(b + 44, (uint64_t)msg.code[1]);
        memcpy(b + 60, " hpc=0x", 7);
        diag_hx(b + 67, st.__pc);
        memcpy(b + 83, " x28=0x", 7);
        diag_hx(b + 90, st.__x[28]);
        Dl_info info;
        uint64_t off = 0;
        const char *sn = "?";
        if (dladdr((void *)st.__pc, &info)) {
            off = st.__pc - (uint64_t)info.dli_fbase;
            if (info.dli_sname) sn = info.dli_sname;
        }
        memcpy(b + 106, " off=0x", 7);
        diag_hx(b + 113, off);
        b[129] = ' ';
        int sl = 0;
        while (sn[sl] && sl < 40) {
            b[130 + sl] = sn[sl];
            sl++;
        }
        mach_vm_address_t raddr = st.__pc;
        mach_vm_size_t rsz = 0;
        vm_region_basic_info_data_64_t rinfo;
        mach_msg_type_number_t ricnt = VM_REGION_BASIC_INFO_COUNT_64;
        mach_port_t robj = MACH_PORT_NULL;
        kern_return_t rkr =
            mach_vm_region(mach_task_self(), &raddr, &rsz, VM_REGION_BASIC_INFO_64, (vm_region_info_t)&rinfo, &ricnt, &robj);
        // Append the GUEST pc (x28 is the reserved CPUREG -> struct cpu*), so a guest `brk`/fault maps to a
        // guest instruction, not just the host JIT pc. Guarded: x28 may not be a cpu ptr outside the cache.
        int bp = 130 + sl;
        memcpy(b + bp, " rkr=0x", 7);
        bp += 7;
        diag_hx8(b + bp, rkr);
        bp += 8;
        if (rkr == KERN_SUCCESS) {
            memcpy(b + bp, " rbase=0x", 9);
            bp += 9;
            diag_hx(b + bp, raddr);
            bp += 16;
            memcpy(b + bp, " rsz=0x", 7);
            bp += 7;
            diag_hx(b + bp, rsz);
            bp += 16;
            memcpy(b + bp, " prot=0x", 8);
            bp += 8;
            diag_hx8(b + bp, (uint32_t)rinfo.protection);
            bp += 8;
            memcpy(b + bp, " rmap=0x", 8);
            bp += 8;
            diag_hx8(b + bp, (st.__pc >= raddr && st.__pc < raddr + rsz) ? 1 : 0);
            bp += 8;
        }
        memcpy(b + bp, " gpc=0x", 7);
        bp += 7;
        struct cpu *bcpu = (struct cpu *)st.__x[28];
        diag_hx(b + bp, bcpu ? bcpu->pc : 0);
        bp += 16;
        memcpy(b + bp, " x16=0x", 7);
        bp += 7;
        diag_hx(b + bp, st.__x[16]);
        bp += 16;
        memcpy(b + bp, " x17=0x", 7);
        bp += 7;
        diag_hx(b + bp, st.__x[17]);
        bp += 16;
        memcpy(b + bp, " x30=0x", 7);
        bp += 7;
        diag_hx(b + bp, st.__lr);
        bp += 16;
        memcpy(b + bp, " x0=0x", 6);
        bp += 6;
        diag_hx(b + bp, st.__x[0]);
        bp += 16;
        memcpy(b + bp, " x1=0x", 6);
        bp += 6;
        diag_hx(b + bp, st.__x[1]);
        bp += 16;
        memcpy(b + bp, " x9=0x", 6);
        bp += 6;
        diag_hx(b + bp, st.__x[9]);
        bp += 16;
        memcpy(b + bp, " x10=0x", 7);
        bp += 7;
        diag_hx(b + bp, st.__x[10]);
        bp += 16;
        for (int r = 2; r <= 8; r++) bp = diag_reg(b, bp, r, st.__x[r]);
        for (int r = 11; r <= 15; r++) bp = diag_reg(b, bp, r, st.__x[r]);
        for (int r = 18; r <= 27; r++) bp = diag_reg(b, bp, r, st.__x[r]);
        extern int jit_pc_in_retained_cache(uint64_t pc);
        memcpy(b + bp, " jit=0x", 7);
        bp += 7;
        diag_hx8(b + bp, jit_pc_in_retained_cache(st.__pc) ? 1 : 0);
        bp += 8;
        extern int jit_hostpc_alias_kind(uint64_t hpc);
        memcpy(b + bp, " alias=0x", 9);
        bp += 9;
        diag_hx8(b + bp, jit_hostpc_alias_kind(st.__pc));
        bp += 8;
        extern void jit_cache_diag(uint64_t *gen, uint64_t *flushes, uint32_t *retired, uint32_t *freed);
        uint64_t cgen = 0, stwf = 0;
        uint32_t nret = 0, nfree = 0;
        jit_cache_diag(&cgen, &stwf, &nret, &nfree);
        memcpy(b + bp, " cgen=0x", 8);
        bp += 8;
        diag_hx(b + bp, cgen);
        bp += 16;
        memcpy(b + bp, " stwf=0x", 8);
        bp += 8;
        diag_hx(b + bp, stwf);
        bp += 16;
        memcpy(b + bp, " ret=0x", 7);
        bp += 7;
        diag_hx8(b + bp, nret);
        bp += 8;
        memcpy(b + bp, " freed=0x", 9);
        bp += 9;
        diag_hx8(b + bp, nfree);
        bp += 8;
        extern int jit_hostpc_lookup(uint64_t hpc, uint64_t *gpc, uint64_t *off, uint32_t *insn);
        uint64_t hgpc = 0, hoff = 0;
        uint32_t hinsn = 0;
        if (jit_hostpc_lookup(st.__pc, &hgpc, &hoff, &hinsn)) {
            memcpy(b + bp, " hblk=0x", 8);
            bp += 8;
            diag_hx(b + bp, hgpc);
            bp += 16;
            memcpy(b + bp, " hoff=0x", 8);
            bp += 8;
            diag_hx(b + bp, hoff);
            bp += 16;
            memcpy(b + bp, " hinsn=0x", 9);
            bp += 9;
            diag_hx8(b + bp, hinsn);
            bp += 8;
        }
        b[bp] = '\n';
        if (write(2, b, bp + 1) < 0) {}
        _exit(139);
    }
    return NULL;
}

static void install_mach_exc(void) {
    if (mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &g_exc_port) != KERN_SUCCESS) return;
    mach_port_insert_right(mach_task_self(), g_exc_port, g_exc_port, MACH_MSG_TYPE_MAKE_SEND);
    task_set_exception_ports(mach_task_self(), EXC_MASK_BAD_ACCESS | EXC_MASK_BAD_INSTRUCTION | EXC_MASK_BREAKPOINT,
                             g_exc_port, EXCEPTION_DEFAULT | MACH_EXCEPTION_CODES, ARM_THREAD_STATE64);
    pthread_t t;
    pthread_create(&t, NULL, exc_thread, NULL);
}

// DD_FAULTCOUNT=1: measurement-only wrapper around nonpie_guard that tallies served low-address faults
// (the guest_base bias-fold's whole point is to drive this to ~0). Per-process; printed at dd_run exit.
static volatile uint64_t g_nonpie_faults;
static volatile uint64_t g_fhist[16];
static const char *g_fhname[16] = {"uoff", "unscaled", "wb",    "regoff", "ldp",   "ldp_wb", "excl",  "lse",
                                   "cas",  "advmult",  "advsi", "dczva",  "ldlit", "ldar",   "other", "x"};

static int fault_class(uint32_t in) {
    if ((in & 0xFFFFFFE0u) == 0xD50B7420u) return 11;                                     // dc zva
    if ((in & 0x3B000000u) == 0x18000000u) return 12;                                     // ldr literal
    if ((in & 0x3A000000u) == 0x28000000u) return ((in >> 23) & 1) ? 5 : 4;               // ldp (wb if op2 odd)
    if ((in & 0x3F200C00u) == 0x38200000u) return 7;                                      // lse atomic
    if ((in & 0x3FA07C00u) == 0x08A07C00u) return 8;                                      // cas
    if ((in & 0x3F200000u) == 0x08000000u) return (in & 0x00800000u) ? 13 : 6;            // ordered(ldar)/exclusive
    if ((in & 0xBFBF0000u) == 0x0C000000u || (in & 0xBFA00000u) == 0x0C800000u) return 9; // advsimd mult
    if ((in & 0xBF000000u) == 0x0D000000u) return 10;                                     // advsimd single
    if ((in & 0x3B000000u) == 0x39000000u) return 0;                                      // uoff
    if ((in & 0x3B200C00u) == 0x38200800u) return 3;                                      // regoff
    if (((in >> 27) & 7) == 7) {
        int m = (in >> 10) & 3;
        return (!((in >> 24) & 1) && (m == 1 || m == 3)) ? 2 : 1; // wb (pre/post) else unscaled/unpriv
    }
    return 14;
}

static void nonpie_guard_count(int sig, siginfo_t *si, void *uc) {
    uint64_t va = (uint64_t)si->si_addr;
    if (g_nonpie_lo && va >= g_nonpie_lo && va < g_nonpie_hi) {
        ucontext_t *u = (ucontext_t *)uc;
        g_fhist[fault_class(*(uint32_t *)(u->uc_mcontext->__ss.__pc))]++;
        uint64_t n = __atomic_add_fetch(&g_nonpie_faults, 1, __ATOMIC_RELAXED);
        if (n % 50000 == 0) {
            char b[256];
            int o = snprintf(b, sizeof b, "[fhist pid=%d n=%llu]", getpid(), (unsigned long long)n);
            for (int i = 0; i < 15; i++)
                if (g_fhist[i])
                    o += snprintf(b + o, sizeof b - o, " %s=%llu", g_fhname[i], (unsigned long long)g_fhist[i]);
            b[o++] = '\n';
            if (write(2, b, o) < 0) {}
        }
    }
    nonpie_guard(sig, si, uc);
}

// fork-server seam (mirrors targets/linux_x86_64.c): the original dd_run inlined (1)
// container init, (2) engine init (signal handlers + pthread key + code-cache arena + env flags), and
// (3) per-launch load+run. The resident ddjitd parent must pay (1)+(2) ONCE and share them COW with
// every forked worker, so those phases are factored into container_init()/engine_global_init().
// engine_global_init() is idempotent (g_engine_inited) so the standalone path is unchanged: standalone
// dd_run() composes container_init -> engine_global_init -> load_program -> run_loaded in the exact
// original order, with the identical operations in each phase.
static int g_engine_inited;

static void container_init(const char *rootfs) {
    // PID ns: only containers (rootfs) get PID 1
    if (rootfs) g_init_hostpid = getpid();
    // cross-engine-process cgroup accounting: a FRESH shared slot table for THIS container init, inherited
    // by every guest fork (see state.c). Per-container so sibling forkserver workers never share a total.
    if (rootfs) acct_container_reset();
    {
        const char *h = getenv("DD_HOSTNAME");
        // ddockerd -> jit config
        if (h && !g_hostname[0]) { strncpy(g_hostname, h, 64); }
        const char *m = getenv("DD_MEM_MAX");
        if (m && !g_mem_max) g_mem_max = parse_size(m);
        const char *p = getenv("DD_PIDS_MAX");
        if (p && !g_pids_max) g_pids_max = dd_parse_id("DD_PIDS_MAX", p);
        container_read_resource_env(); // docker --cpus / --read-only / --ulimit (DD_CPUS/DD_ROOTFS_RO/DD_ULIMITS)
        const char *pub = getenv("DD_PUBLISH");
        if (pub && !g_nportmap) parse_publish(pub);
        const char *low = getenv("DD_LOWER");
        if (low && !g_nlower) {
            char tb[8192];
            // ro lowers, colon-sep (highest first)
            snprintf(tb, sizeof tb, "%s", low);
            char *sv = NULL;
            for (char *t = strtok_r(tb, ":", &sv); t; t = strtok_r(NULL, ":", &sv))
                add_lower(t);
        }
        // Private-loopback netns: derive g_netns from the DD_NETNS KEY (set per-container by the
        // daemon; a `docker exec` reuses the target container's key so the exec shares its 127.0.0.1).
        // When DD_NETNS is UNSET (a bare engine launch / the basics matrix), MINT a per-process key and
        // export it — so loopback isolation is ALWAYS on. Otherwise g_netns stayed empty, lo_on() was
        // false, and 127.0.0.1 fell through to the REAL host TCP stack shared by every concurrent guest,
        // so two containers' 127.0.0.1:PORT collided (nc-loopback intermittently reached another
        // container's published listener). Mirrors linux_x86_64.c. Opt out via DD_NONETNS.
        if (!getenv("DD_NONETNS") && !g_netns[0]) {
            const char *nn = getenv("DD_NETNS");
            char key[40];
            if (nn && nn[0])
                snprintf(key, sizeof key, "%.39s", nn);
            else
                snprintf(key, sizeof key, "%d", (int)getpid());
            snprintf(g_netns, sizeof g_netns, "/tmp/.ddnet-%.40s", key);
            // Export the minted key so children/exec + abstract-AF_UNIX/IPC/bridge share this
            // container's namespace (getenv("DD_NETNS")); a daemon-supplied key is already in the env.
            if ((mkdir(g_netns, 0700) == 0 || errno == EEXIST) && !(nn && nn[0])) setenv("DD_NETNS", key, 1);
        }
        const char *eu = getenv("DD_UID");
        if (eu && g_uid < 0) g_uid = dd_parse_id("DD_UID", eu);
        const char *eg = getenv("DD_GID");
        if (eg && g_gid < 0) g_gid = dd_parse_id("DD_GID", eg);
        // USER ns (process.user)
    }
    if (rootfs && rootfs[0]) {
        g_rootfs = (char *)rootfs;
        if (!realpath(g_rootfs, g_rootfs_canon)) snprintf(g_rootfs_canon, sizeof g_rootfs_canon, "%s", g_rootfs);
        // the immutable jail boundary for secure_resolve
        g_rootfs_canon_len = strlen(g_rootfs_canon);
        // pinned root for the per-component resolver
        g_root_fd = open(g_rootfs_canon, O_RDONLY | O_DIRECTORY);
        g_root_fd = engine_fd_hoist(g_root_fd); // keep it off the guest's low fds (else it squats fd 3)
        container_populate_dev();        // /dev/{fd,stdin,stdout,stderr,ptmx,pts,shm,console,...} the unpacker stripped
        container_populate_machine_id(); // /etc/machine-id agreeing with boot_id (if image ships none)
        if (g_uid < 0) g_uid = 0;
        // container default: run as root (0); unless DD_UID/--uid set
        if (g_gid < 0) g_gid = 0;
        // docker -w / initial working directory: start the guest in DD_CWD (must be reachable inside the
        // container -- typically a bind-mounted volume). confine() normalizes + clamps it to the rootfs.
        const char *icwd = getenv("DD_CWD");
        if (icwd && icwd[0]) confine(icwd, g_cwd, sizeof g_cwd);
    }
    // bind-mount volumes: "[ro:]guestpath:hostdir,..." -- delegate to add_vol() (the shared vfs.c parser)
    // so the optional `ro:` read-only marker is handled in ONE place for both engines. Ingested regardless
    // of whether a rootfs is set (matching linux_x86_64.c): add_vol opens the HOST dir directly and the
    // registration is what surfaces the bind in /proc/self/mountinfo + /proc/mounts.
    const char *vspec = getenv("DDVOL");
    if (vspec && vspec[0]) {
        char tmp[4096];
        snprintf(tmp, sizeof tmp, "%s", vspec);
        char *sv = NULL;
        for (char *t = strtok_r(tmp, ",", &sv); t; t = strtok_r(NULL, ",", &sv))
            add_vol(t);
    }
    // derive the run user's supplementary group set from the image rootfs (runc additionalGids), after
    // g_uid/g_gid + the overlay lowers are resolved, so getgroups(2) and /proc/self/status Groups: report it.
    if (g_rootfs) container_parse_groups();
}

// idempotent engine init (fault handlers + pthread key + code-cache arena + env-flag reads).
// Returns 0 on success, nonzero exit code on failure. First call wins; later calls are no-ops
// (g_engine_inited), so the resident fork-server parent pays this once and the standalone path runs it
// exactly as before.
static int engine_global_init(void) {
    if (g_engine_inited) return 0;
    if (getenv("CRASHDBG")) {
        // SA_ONSTACK + an alternate signal stack so the handler survives a corrupted/overflowed guest
        // stack (otherwise diag_crash double-faults and the process is signal-killed before it can report).
        static char altstk[64 * 1024];
        stack_t ss = {.ss_sp = altstk, .ss_size = sizeof altstk, .ss_flags = 0};
        sigaltstack(&ss, NULL);
        struct sigaction sa;
        memset(&sa, 0, sizeof sa);
        sa.sa_sigaction = diag_crash;
        sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
        sigaction(SIGSEGV, &sa, NULL);
        sigaction(SIGBUS, &sa, NULL);
        // Also catch SIGTRAP so a guest `brk #imm` (e.g. a chromium/crashpad Check-failed abort compiled to
        // a trap, or a __builtin_trap) dumps the guest PC/context instead of silently signal-killing the
        // engine host-side. Diagnostic-only (CRASHDBG); deliver_guest_fault still hands a trap the guest owns
        // to its own handler first, so this only reports an otherwise-fatal, unhandled trap.
        sigaction(SIGTRAP, &sa, NULL);
        install_mach_exc();
    } else {
        // Normal runs: serve a non-PIE ET_EXEC's absolute DATA refs (baked at the low link vaddr) at +bias
        // and resume. Inert for PIE/static-PIE (nonpie_guard checks g_nonpie_lo, set only for ET_EXEC by
        // load_elf below); a fault it doesn't own re-raises with the default action.
        struct sigaction sa;
        memset(&sa, 0, sizeof sa);
        sa.sa_sigaction = getenv("DD_FAULTCOUNT") ? nonpie_guard_count : nonpie_guard;
        sa.sa_flags = SA_SIGINFO | SA_ONSTACK; // run on the per-thread altstack so a guest stack
                                               // overflow's guard fault is deliverable (host SP == guest SP)
        sigaction(SIGSEGV, &sa, NULL);
        sigaction(SIGBUS, &sa, NULL);
    }
    // verify the purity gate, PoC-style: pure->1, impure->0
    if (getenv("TIER2_SELFTEST")) {
        // add x0,x0,x1; mul x0,x0,x2; ret
        uint32_t pure[] = {0x8b010000u, 0x9b027c00u, 0xd65f03c0u};
        // ldr x1,[x0]; add; ret
        uint32_t impure[] = {0xf9400001u, 0x8b010000u, 0xd65f03c0u};
        // str x1,[x0]; ret
        uint32_t sidef[] = {0xf9000001u, 0xd65f03c0u};
        fprintf(stderr, "[tier2] purity gate: pure=%d(want1) load=%d(want0) store=%d(want0)\n", region_pure(pure, 3),
                region_pure(impure, 3), region_pure(sidef, 2));
        struct cpu sc;
        sc.ssp = 0;
        // §B shadow-stack mechanism
        int errs = 0, fast = 0;
        for (int d = 0; d < 200; d++)
            // 200 nested calls
            shadow_push(&sc, 0x1000 + d, 0);
        for (int d = 199; d >= 0; d--)
            (shadow_classify(&sc, 0x1000 + d) == SS_FAST) ? fast++ : errs++;
        sc.ssp = 0;
        shadow_push(&sc, 0xA, 0);
        shadow_push(&sc, 0xB, 0);
        shadow_push(&sc, 0xC, 0);
        // longjmp past B,C -> A
        int unwind = (shadow_classify(&sc, 0xA) == SS_UNWIND);
        // computed return
        int foreign = (shadow_classify(&sc, 0xDEAD) == SS_FOREIGN);
        fprintf(stderr, "[tier2] shadow stack: fast=%d/200 errs=%d(want0) unwind=%d(want1) foreign=%d(want1)\n", fast,
                errs, unwind, foreign);
    }
    if (pthread_key_create(&g_cpu_key, NULL) != 0) {
        perror("pthread_key_create");
        return 1;
    }
    if (g_cpu_key >= 4096) {
        fprintf(stderr, "[jit] TSD key %u too large for inline ldr\n", (unsigned)g_cpu_key);
        return 1;
    }

    // Code cache. Default: dual-mapped RW/RX so the engine never toggles W^X. Allocate a
    // plain anon RW region (the writer alias = g_cache) and vm_remap the SAME physical pages
    // to a second address that we mark RX (the executor alias). Writes through g_cache become
    // visible to execution at g_cache+g_rw2rx after an icache flush, with NO per-region
    // pthread_jit_write_protect_np() flip. NODUALMAP=1 reverts to a single MAP_JIT mapping.
    if (!getenv("NODUALMAP")) {
        uint8_t *rw;
        ptrdiff_t d;
        if (dualmap_alloc(&rw, &d) == 0) {
            g_cache = rw;
            g_rw2rx = d;
            g_dualmap = 1;
        } else {
            fprintf(stderr, "[jit] dual-map unavailable -> W^X-toggle fallback\n");
        }
    }
    if (!g_dualmap) {
        g_cache = mmap(NULL, CACHE_SZ, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANON | MAP_JIT, -1, 0);
        if (g_cache == MAP_FAILED) {
            perror("mmap jit");
            return 1;
        }
    }
    g_cp = g_cache;

    g_trace = getenv("JT") != NULL;
    g_systrace = getenv("JTS") != NULL;
    g_dbg_nochain = getenv("DDDBG_NOCHAIN") != NULL; // debug-only: force every block through the dispatcher
    g_dbg_gprdump = getenv("DDDBG_GPRDUMP") != NULL; // debug-only: dump all GPRs per block (register differential)
    g_prof = getenv("PROF") != NULL;
    g_no_stw_reclaim = getenv("NOSTWRECLAIM") != NULL;
    // A1: steal host x16/x17 for the engine (default on). NOSTEAL1617=1 -> legacy 3-reg stolen set
    // (guest x16/x17 in host regs, per-branch red-zone stash/restore). Read once before any translation.
    if (getenv("NOSTEAL1617")) g_steal1617 = 0;
    // IBSLIM: NOIBSLIM=1 reverts the dispatch/call-path slimming (legacy emit_set_x30 +
    // the per-site IC at recognized interpreter-dispatch `br`s) for A/B.
    if (getenv("NOIBSLIM")) g_noibslim = 1;
    // IRQSLIM: fixed 2-insn poll header + poll-free forward-chain entry at body+8
    // (every cycle still polls via its backward/indirect edge -- see cache.c). Requires the steal
    // layout and an emitted poll. NOIRQSLIM=1 -> legacy poll-on-every-entry for A/B.
    if (g_steal1617 && !getenv("NOIRQSLIM") && !getenv("NOIRQCHECK")) g_fwdskip = 8;
    // guest_base bias-fold (non-PIE ET_EXEC): default on; NOGUESTFOLD=1 reverts to the SIGSEGV-per-low-
    // access fault path (A/B kill-switch). Read once before any translation; inert for PIE (gated on the
    // non-PIE marker g_nonpie_lo, set only for ET_EXEC by load_elf).
    if (getenv("NOGUESTFOLD")) g_guestfold = 0;
    if (getenv("NOMTIBTC")) g_mtibtc = 0; // W5C: disable race-free threaded IBTC fill (A/B kill-switch)
    if (getenv("NOFUTEXQ")) g_futexq = 0; // W5C: disable per-address futex wait queues (A/B kill-switch)
    // Untrusted-guest isolation (the sentry process-split). OFF by default -> trusted path unchanged.
    g_untrusted = getenv("DDJIT_UNTRUSTED") != NULL;
    g_sentry_sandbox = getenv("DDJIT_SANDBOX") != NULL;
    // pcache_poison_check runs AFTER the codegen-mode flags above so it can refuse to persist an arena
    // that a non-default mode (PROF) baked unrecorded host pointers
    // into. (g_pcache itself is read at the top of dd_run -- per-invocation, mirroring linux_x86_64.c,
    // so a fork-server runner honors the CLIENT's DDJIT_PCACHE; the check is mode-only and independent.)
    pcache_poison_check();
    // ptrace tracer/tracee coordination arena -- mmap the shared region ONCE here, BEFORE any guest
    // fork, so every descendant guest process inherits the same physical pages. Inert until a guest ptraces.
    ptrace_arena_init();
    // Arm the SIGUSR1 checkpoint control handler when DDJIT_CHECKPOINT_DIR is set (harmless no-op otherwise).
    // Runs in every process (init + forked children) so the whole tree is checkpointable.
    ckpt_control_init();
    // Host-IOSurface GPU bridge (--gui): force its one-time ObjC/CoreFoundation/Foundation/IOSurface class
    // inits to completion HERE, single-threaded and BEFORE any guest thread/fork, so a lazy +initialize can
    // never be mid-flight when Chrome forks its zygote/broker (which would abort the child via libobjc's
    // fork-safety guard -> chromium EXIT=137). Gated on DD_GPU_IOSURFACE; a no-op for every other workload.
    dd_gpu_prewarm_fork_safety();
    g_engine_inited = 1;
    return 0;
}

// load main program + (optional) interp, recording the load base/entry/at_base into *lm/*li.
// Used both by the standalone path and by the fork-server's parent preload (so the COW-inherited image
// is byte-identical and the warm worker re-runs from the same entry at the same base). The gb/pb/ib
// buffers are static because g_exe_path = prog points into gb and must outlive this call.
static const char *load_program(const char *prog, struct loaded *lm, struct loaded *li, uint64_t *jump,
                                uint64_t *at_base, int *have_interp) {
    // cache id keys the INVOKED name (argv[0] pre-resolution), exactly as before this refactor.
    const char *argv0 = prog;
    static char gb[1024];
    prog = find_in_path(prog, gb, sizeof gb); // bare "sh" (docker) -> "/bin/sh" via the container PATH
    g_exe_path = prog;
    // /proc/self/exe must be the ABSOLUTE, CANONICAL guest path of the loaded image: a RELATIVE guest
    // invocation ("./x" from a harness) or an entry symlink (/bin/sh->busybox) otherwise leaks into the
    // link value, and glibc static-pie ASSERTS on it at startup ("dl-origin.c: linkval[0]=='/'"). Done
    // HERE (not only in dd_run) so the fork-server's parent preload -- which calls load_program directly,
    // NOT dd_run -- also hands every COW warm worker a canonical g_exe_path (#378: byte-identical to a
    // cold launch). Static: the value must outlive this call, like gb above. Mirrors linux_x86_64.c.
    static char bootexe[4200];
    exe_canon(prog, bootexe, sizeof bootexe);
    g_exe_path = bootexe;
    static char pb[4200];
    const char *prog_host =
        // resolve through the overlay (upper, then lowers) + follow the entry symlink (/bin/sh->busybox)
        xresolve_overlay(prog, pb, sizeof pb);
    // when the persistent cache is on, map the image + interp at FIXED VAs so the translated arena
    // (block-map keys + baked guest addresses) is byte-identical across runs -> reusable from the cache.
    if (g_pcache) g_force_base = PC_IMG_BASE;
    {
        const char *e = getenv("DDDBG_IMGBASE");
        if (e) g_force_base = strtoull(e, 0, 16);
    } // debug: pin img base (trace alignment)
    load_elf(prog_host, lm);

    // Dynamic: load the PT_INTERP (ld.so) and enter THERE; it loads libs + relocates.
    *jump = lm->entry;
    *at_base = 0;
    *have_interp = 0;
    const char *interp_host = NULL;
    char interp[256];
    if (elf_interp(prog_host, interp, sizeof interp) == 0) {
        static char ib[4200];
        // follow+confine ld.so symlink (through the overlay)
        interp_host = xresolve_overlay(interp, ib, sizeof ib);
        if (g_pcache) g_force_base = PC_INTERP_BASE;
        {
            const char *e = getenv("DDDBG_INTERPBASE");
            if (e) g_force_base = strtoull(e, 0, 16);
        } // debug: pin interp base
        load_elf(interp_host, li);
        *jump = li->entry;
        *at_base = li->base;
        *have_interp = 1;
    }
    // key the cache by the identity of the guest binary + interp (+ the invoked name).
    if (g_pcache) g_pc_binid = pcache_make_id(prog_host, interp_host, argv0);
    return prog;
}

// fresh per-launch guest run from a loaded image. Allocates a private heap + a guest stack +
// cpu and runs from `jump`. Shared by dd_run (standalone/cold) and the fork-server's warm worker (which
// restores a pristine COW image first, then calls this against the parent-preloaded base). Body is the
// original dd_run tail verbatim, so standalone behavior is byte-identical.
static int run_loaded(int argc, char *const argv[], struct loaded *lm, uint64_t jump, uint64_t at_base) {
    // checkpoint/restore: place the brk heap in the deterministic high arena (0 hint => normal NULL placement)
    uint8_t *heap =
        mmap((void *)ckpt_place_hint(256u << 20), 256u << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    gmap_add((uint64_t)heap, 256u << 20); // track so a later execve() can reclaim the inherited heap
    brk_lo = brk_cur = (uint64_t)heap;
    brk_hi = brk_lo + (256u << 20);

    struct cpu c;
    memset(&c, 0, sizeof c);
    // guest argv = prog + its args
    c.sp = build_stack(argc, (char **)argv, lm, at_base);
    c.pc = jump;

    proc_reg_publish(g_exe_path, argc, argv); // publish this process into the /proc table
    if (g_untrusted) sentry_init();           // fork the host-authority sentry + (optionally) confine the worker
    run_guest(&c);
    if (g_untrusted) sentry_shutdown(); // signal quit + waitpid (reap, no orphan)
    return c.exit_code;
}

// Restore driver (the dd_jit::Runtime::restore surface; also reachable via the `--restore <dir>` flag).
// Rebuilds the checkpointed process tree from `dir` and resumes it. Guest memory for the init is rebuilt
// FIRST -- before container_init/engine_global_init allocate anything -- inside ckpt_restore_tree.
int dd_restore(const char *rootfs, const char *dir) {
    g_pcache = getenv("DDJIT_PCACHE") != NULL && getenv("DDJIT_NOPCACHE") == NULL;
    return ckpt_restore_tree(rootfs, dir);
}

int dd_run(const char *rootfs, int argc, char *const argv[]) {
    // Resume a previously checkpointed workspace instead of launching a program (the GUI sets this on
    // window reopen; the container config/env is otherwise identical to the original launch).
    const char *rdir = getenv("DDJIT_RESTORE_DIR");
    if (rdir && rdir[0]) return dd_restore(rootfs, rdir);
    if (argc < 1 || !argv || !argv[0]) return 2;
    // persistent cross-process translated-code cache. Landed OPT-IN (DDJIT_PCACHE=1, same gating as
    // the x86 opt8 pcache) so the default correctness matrix stays byte-identical to the baseline; the
    // one-line flip to default-on (`getenv("DDJIT_NOPCACHE") == NULL`) is gated on a green full pcache-on
    // matrix soak (see plan). DDJIT_NOPCACHE=1 is the kill-switch and always wins. Read here (per
    // dd_run, like linux_x86_64.c) so a fork-server cold runner honors the CLIENT's env.
    g_pcache = getenv("DDJIT_PCACHE") != NULL && getenv("DDJIT_NOPCACHE") == NULL;
    g_coldprof = getenv("COLDPROF") != NULL;
    container_init(rootfs);
    int irc = engine_global_init();
    if (irc) return irc;
    const char *prog = argv[0];
    static char gb[1024];
    prog = find_in_path(prog, gb, sizeof gb); // bare "sh" (docker) -> "/bin/sh" via the container PATH
    set_guest_comm(prog); // Linux comm = basename of the exec NAME (stays the script's for a shebang entry)
    g_exe_path = prog;
    char pb[4200];
    const char *prog_host =
        // resolve through the overlay (upper, then lowers) + follow the entry symlink (/bin/sh->busybox)
        xresolve_overlay(prog, pb, sizeof pb);

    // Initial-exec shebang handling -- mirror of execve case 221 via the shared resolve_shebang_chain()
    // helper. The container entry may itself be a "#!" script (e.g. postgres' docker-entrypoint.sh), AND
    // that script's interpreter may ITSELF be a "#!" script (mysql:8's #!/usr/bin/env bash -> /usr/bin/env
    // is #!/usr/bin/coreutils ...). load_elf has no ELF-magic/#! check, so it would parse the script text
    // as a bogus ELF and fault before any guest syscall runs. Resolve the whole chain (Linux binfmt_script,
    // up to SHEBANG_MAX nested levels), rewriting argv to [finalInterp, ..., scriptpath, args...] and
    // loading the FINAL interpreter. A missing/non-shebang ELF falls straight through unchanged below.
    char sb_store[SHEBANG_MAX * 2][256];
    char *sb_argv[256];
    int sb_argc = 0;
    sb_argv[sb_argc++] = (char *)prog; // `prog` is find_in_path-resolved; the chain keeps it as scriptpath
    for (int i = 1; i < argc && sb_argc < 255; i++)
        sb_argv[sb_argc++] = (char *)argv[i];
    sb_argv[sb_argc] = NULL;
    const char *sb_finalhost;
    char sb_fhb[4200];
    int sb_new =
        resolve_shebang_chain(sb_argv, sb_argc, 256, prog_host, sb_store, sb_fhb, sizeof sb_fhb, &sb_finalhost);
    if (sb_new < 0) {
        fprintf(stderr, "dd: too many nested #! interpreters (ELOOP): %s\n", prog);
        return 40; // ELOOP
    }
    if (sb_new != sb_argc) { // a shebang chain resolved -> run the final interpreter, not the script
        argc = sb_new;
        argv = (char *const *)sb_argv;
        // load_program below re-resolves argv[0] and re-sets g_exe_path to the binary actually loaded
        // (matches /proc/self/exe for a script exec).
    }
    // /proc/self/exe canonicalization now happens inside load_program (below), so it also covers the
    // fork-server's parent preload path (which never calls dd_run) -- see #378. load_program re-resolves
    // argv[0] and sets g_exe_path to the canonical absolute path of the binary actually loaded, matching
    // /proc/self/exe for a script exec.
    struct loaded lm, li;
    uint64_t jump, at_base;
    int have_interp;
    load_program(argv[0], &lm, &li, &jump, &at_base, &have_interp); // (sets g_pc_binid + fixed bases when g_pcache)
    // try to restore the arena from the persistent cache.
    if (g_pcache) {
        g_pc_entry = jump;
        int hit = pcache_load(jump); // graceful MISS on any stale/corrupt/truncated cache -> translate fresh
        if (g_coldprof) fprintf(stderr, "[pcache] %s reloc=%d\n", hit ? "HIT" : "MISS", g_nreloc);
    }
    int ec = run_loaded(argc, argv, &lm, jump, at_base);
    pcache_save(); // persist on a cold miss (guest exit via case 93 returns here; case 94 saves + _exit)
    if (getenv("DD_FAULTCOUNT"))
        fprintf(stderr, "[faultcount] pid=%d nonpie_served=%llu\n", getpid(), (unsigned long long)g_nonpie_faults);
    return ec;
}

// resident ddjitd fork-server (server/client/worker), SHARED with linux_x86_64.c through the
// container_init/engine_global_init/load_program/run_loaded/dd_run seam above. aarch64 has no
// g_loadbase and its container model never chdir()s the engine into the rootfs (those knobs stay
// default no-ops), but its load_elf applies per-segment W^X to the guest image (.text R+X, .rodata R;
// the prewarm run's guest may mprotect more, e.g. musl RELRO) -- so the fork-server's pristine-image
// restore must open the span RW and re-apply the PRISTINE load-time protections afterwards, mirroring
// load_elf's p_flags loop. Without this the first restore memcpy SIGBUSes on the R+X .text.
static void fsrv_restore_prep_a64(const struct loaded *L, uint64_t span) {
    mprotect((void *)L->base, span, PROT_READ | PROT_WRITE);
}

static void fsrv_restore_done_a64(const struct loaded *L, uint64_t span) {
    (void)span;
    const uint8_t *ph = (const uint8_t *)L->phdr;
    uint64_t minv = ~0ull;
    for (int i = 0; i < L->phnum; i++) {
        const uint8_t *p = ph + (size_t)i * (size_t)L->phent;
        if (rd32(p) != 1) continue; // PT_LOAD
        uint64_t v = rd64(p + 16);
        if (v < minv) minv = v;
    }
    uint64_t bias = L->base - (minv & ~0xFFFull); // load_elf: bias = host base - link basepage
    for (int i = 0; i < L->phnum; i++) {
        const uint8_t *p = ph + (size_t)i * (size_t)L->phent;
        if (rd32(p) != 1) continue;
        uint32_t fl = rd32(p + 4); // PF_X=1, PF_W=2, PF_R=4
        uint64_t v = rd64(p + 16), msz = rd64(p + 40);
        uint64_t s = (v + bias) & ~0xFFFull, e = (v + bias + msz + 0xFFFull) & ~0xFFFull;
        int prot = PROT_READ | ((fl & 2) ? PROT_WRITE : 0) | ((fl & 1) ? PROT_EXEC : 0);
        if (e > s) mprotect((void *)s, e - s, prot);
    }
}

#define FSRV_RESTORE_PREP(L, span) fsrv_restore_prep_a64((L), (span))
#define FSRV_RESTORE_DONE(L, span) fsrv_restore_done_a64((L), (span))
#include "../os/linux/forkserver.c"

#ifndef DDJIT_LIB
// The engine entry point. Named `ddjit_entry` so the runtime can be linked as a library and launched
// by an in-process fork()+call; the thin `main` shim below keeps the standalone binary (used by the test
// harness) launching identically. The static-lib build defines DDJIT_NO_MAIN to drop the shim.
int ddjit_entry(int argc, char **argv);
#ifndef DDJIT_NO_MAIN
int main(int argc, char **argv) {
    return ddjit_entry(argc, argv);
}
#endif
int ddjit_entry(int argc, char **argv) {
    int ai = 1;
    const char *rootfs = NULL;
    // typed-config launch (the daemon's default path): `--configfd <fd>` streams a serialized ddjit_config
    // over the inherited fd instead of the DD_* env/flag dialect. Dispatched before all other flags.
    if (argc > 2 && strcmp(argv[1], "--configfd") == 0) return ddjit_run_configfd(atoi(argv[2]));
    if (argc > 2 && strcmp(argv[1], "--configfile") == 0) return ddjit_run_configfile(argv[2]);
    // fork-server dispatch (gated; standalone path untouched when neither flag is present):
    //   --server SOCK [--rootfs DIR] [--prewarm PROG] : run resident ddjitd, listen on SOCK
    //   --client SOCK [--rootfs DIR] PROG [args...]   : forward a launch request to a ddjitd
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--server") == 0) return ddjitd_server_main(argc, argv);
        if (strcmp(argv[i], "--client") == 0) return ddjitd_client_main(argc, argv);
    }
    // container flags (SentryConfig)
    while (ai < argc && argv[ai][0] == '-' && argv[ai][1] == '-') {
        if (!strcmp(argv[ai], "--rootfs") && ai + 1 < argc) {
            rootfs = argv[ai + 1];
            ai += 2;
        } else if (!strcmp(argv[ai], "--restore") && ai + 1 < argc) {
            setenv("DDJIT_RESTORE_DIR", argv[ai + 1], 1); // dd_run() dispatches to dd_restore() (no <elf> arg)
            ai += 2;
        } else if (!strcmp(argv[ai], "--hostname") && ai + 1 < argc) {
            strncpy(g_hostname, argv[ai + 1], 64);
            ai += 2;
        } else if (!strcmp(argv[ai], "--mem-max") && ai + 1 < argc) {
            g_mem_max = parse_size(argv[ai + 1]);
            ai += 2;
        } else if (!strcmp(argv[ai], "--pids-max") && ai + 1 < argc) {
            g_pids_max = dd_parse_id("--pids-max", argv[ai + 1]);
            ai += 2;
        } else if (!strcmp(argv[ai], "--publish") && ai + 1 < argc) {
            parse_publish(argv[ai + 1]);
            ai += 2;
        } else if (!strcmp(argv[ai], "--lower") && ai + 1 < argc) {
            add_lower(argv[ai + 1]);
            ai += 2;
            // ro overlay lower layer
        } else if (!strcmp(argv[ai], "--netns") && ai + 1 < argc) {
            snprintf(g_netns, sizeof g_netns, "/tmp/.ddnet-%.40s", argv[ai + 1]);
            mkdir(g_netns, 0700);
            ai += 2;
            // private loopback ns
        } else if (!strcmp(argv[ai], "--uid") && ai + 1 < argc) {
            g_uid = dd_parse_id("--uid", argv[ai + 1]);
            ai += 2;
            // USER ns uid
        } else if (!strcmp(argv[ai], "--gid") && ai + 1 < argc) {
            g_gid = dd_parse_id("--gid", argv[ai + 1]);
            ai += 2;
        } else
            break;
    }
    if (getenv("DDJIT_RESTORE_DIR")) return dd_run(rootfs, 0, NULL); // resume a checkpoint (no <elf> needed)
    if (ai >= argc) {
        fprintf(stderr,
                "usage: %s [--rootfs DIR] [--hostname NAME] [--mem-max BYTES] [--pids-max N] [--publish H:C] "
                "<aarch64-elf> [args...]\n"
                "       %s [--rootfs DIR] --restore <checkpoint-dir>\n"
                "  checkpoint a running guest: launch with DDJIT_CHECKPOINT_DIR=<dir> and send it SIGUSR1\n",
                argv[0], argv[0]);
        return 2;
    }
    return dd_run(rootfs, argc - ai, argv + ai);
}
#endif
