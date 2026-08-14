#include "namespace.h"
#include "../checkpoint_channel.c"
#include "../bus.h"
#include "../../linux_abi/dns.h"

// HL engine: the aarch64 Linux-guest JIT runner (unity translation unit).
//
// A same-ISA aarch64->aarch64 JIT services the guest's Linux syscalls in userspace (no VM). This TU
// pulls in the engine (jit/), the aarch64 guest frontend (frontend/aarch64/), the Linux personality +
// container layer (os/linux/), and defines hl_run_linux_guest() plus main(). The x86-64
// The guest reuses the Linux ABI and translator layers with the selected frontend.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <fcntl.h>
#include "../../linux_abi/host_mman.h" // <sys/mman.h>, or the typed VM seam where the host has none
#include <sys/stat.h>
#include <pthread.h>
#include <errno.h>
#include <limits.h>
#include <time.h>
#include <sys/time.h>
#include "../../linux_abi/host_uio.h" // <sys/uio.h>, or the guest iovec layout where the host has none
#include "../../linux_abi/host_socket.h"
#include "../../linux_abi/host_socket.h"
#include "../../linux_abi/host_socket.h"
#include "../../linux_abi/host_socket.h"
#include "../../linux_abi/host_poll.h" // <poll.h>, or a typed absence where the host has no mixed-handle readiness
#include "../../host/native_compat.h"
#include "../../host/native_context.h"
#include <signal.h>
#include "../../linux_abi/host_dirent.h"
#if defined(__APPLE__)
#include <mach/mach.h>
#include <mach/mach_vm.h> // Mach exception diagnostics; JIT mappings belong to src/host/macos
#include <sys/event.h>
#endif
#include "../../linux_abi/host_tty.h"
#include "../../linux_abi/host_tty.h"
#include "../../linux_abi/host_proc.h"
#include "../../linux_abi/host_wait.h"
#include "../../linux_abi/device.h"
#include "../../host/process.h"
#include <stdatomic.h>
#if !defined(_WIN32)
#include <dlfcn.h>
#endif

#include "hl/engine.h"
#include "hl/linux_abi.h"
#include "hl/log.h"
#include "hl/syscall_trap.h"
#include "../options.h"
#include "native.h"
#include "services.h"
#include "../bus.h"
#include "../result.h"
#include "../../linux_abi/bus.h"
#include "../../host/range.h"

/* Instance-scoped host seam supplied by hl_engine. CLI launches retain their native-host path with NULL. */
static hl_target_services g_target_services;
#define g_host_services (g_target_services.injected)
#define g_jit_services (g_target_services.bound)
static hl_status g_engine_result_status;
static hl_linux_abi *g_linux_box;
static void *g_syscall_trap_context;
static hl_syscall_trap_fn g_syscall_trap_dispatch;
static _Atomic int g_runtime_credentials_published;

void hl_target_syscall_trap_install(void *context, hl_syscall_trap_fn dispatch) {
    g_syscall_trap_context = context;
    g_syscall_trap_dispatch = dispatch;
    atomic_store_explicit(&g_runtime_credentials_published, 0, memory_order_release);
}

struct cpu;
static int hl_target_syscall_trap(struct cpu *c);
static void sentry_trapped_exit(void);
static int hl_target_credentials_publish(struct cpu *c);
static int jit_guest_soft_activate(void);
static void jit_guest_soft_restore_activate(void);
static void jit_guest_soft_restore_deactivate(void);
static void jit_guest_soft_deactivate(void);
static int jit_guest_soft_active(void);
static int gna_hit(uint64_t address, uint64_t length);
static void gna_filter(uint64_t *first, uint64_t *last);

hl_status hl_run_linux_guest_status(void) {
    return g_engine_result_status;
}

static uint64_t g_host_launch_monotonic_ns;
static void filemap_refresh_emulated(uint64_t lo, uint64_t hi);

#include "../../translator/guest/aarch64/cpu.h"
static int container_pid(void);

static int hl_target_syscall_trap(struct cpu *c) {
    hl_syscall_cpu_aarch64 snapshot;
    hl_syscall_trap_result result;
    int32_t status;
    if (g_syscall_trap_dispatch == NULL) return 0;
    if (c->x[8] >= 174 && c->x[8] <= 177 &&
        !atomic_load_explicit(&g_runtime_credentials_published, memory_order_acquire) &&
        !hl_target_credentials_publish(c)) {
        c->exited = 1;
        c->exit_code = 127;
        return 1;
    }
    memset(&snapshot, 0, sizeof(snapshot));
    snapshot.abi = HL_SYSCALL_TRAP_ABI;
    snapshot.size = sizeof(snapshot);
    memcpy(snapshot.x, c->x, sizeof(snapshot.x));
    snapshot.sp = c->sp;
    snapshot.pc = c->pc;
    snapshot.tls = c->tls;
    snapshot.nzcv = c->nzcv;
    snapshot.task = (uint64_t)(c->tid ? c->tid : container_pid());
    memset(&result, 0, sizeof(result));
    result.abi = HL_SYSCALL_TRAP_ABI;
    result.size = sizeof(result);
    status = g_syscall_trap_dispatch(g_syscall_trap_context, HL_GUEST_ISA_AARCH64, &snapshot, &result);
    if (status != 0 || result.abi != HL_SYSCALL_TRAP_ABI || result.size < sizeof(result) ||
        result.outcome == HL_SYSCALL_TRAP_DECLINED)
        return 0;
    memcpy(c->x, snapshot.x, sizeof(snapshot.x));
    c->sp = snapshot.sp;
    c->pc = snapshot.pc;
    c->tls = snapshot.tls;
    c->nzcv = snapshot.nzcv;
    if (result.outcome == HL_SYSCALL_TRAP_EXIT) {
        sentry_trapped_exit();
        c->exited = 1;
        c->exit_code = result.exit_status;
    } else if (result.outcome == HL_SYSCALL_TRAP_FAULT) {
        c->exited = 1;
        c->exit_code = 127;
    } else if (result.outcome == HL_SYSCALL_TRAP_REPLACE_IMAGE) {
        c->redirect = 1;
    } else if (result.outcome != HL_SYSCALL_TRAP_CONTINUE) {
        return 0;
    }
    return 1;
}

static int hl_target_task_event(struct cpu *c, uint64_t event, uint64_t value, uint64_t source, uint64_t child) {
    hl_syscall_cpu_aarch64 snapshot;
    hl_syscall_trap_result result;
    if (g_syscall_trap_dispatch == NULL) return 1;
    memset(&snapshot, 0, sizeof(snapshot));
    snapshot.abi = HL_SYSCALL_TRAP_ABI;
    snapshot.size = sizeof(snapshot);
    snapshot.x[0] = value;
    snapshot.x[1] = source;
    snapshot.x[2] = child;
    snapshot.x[8] = event;
    snapshot.task = source;
    memset(&result, 0, sizeof(result));
    result.abi = HL_SYSCALL_TRAP_ABI;
    result.size = sizeof(result);
    int32_t status = g_syscall_trap_dispatch(g_syscall_trap_context, HL_GUEST_ISA_AARCH64, &snapshot, &result);
    return status == 0 && result.abi == HL_SYSCALL_TRAP_ABI && result.size >= sizeof(result) &&
           (result.outcome == HL_SYSCALL_TRAP_CONTINUE || result.outcome == HL_SYSCALL_TRAP_DECLINED);
}

static int hl_target_syscall_trap_selected(const struct cpu *c) {
    /* Rust-owned exit and process identity. Keep selection cheaper than constructing the stable snapshot. */
    return g_syscall_trap_dispatch != NULL && (c->x[8] == 93 || (c->x[8] >= 172 && c->x[8] <= 178));
}

#include "../../translator/guest/aarch64/signal.h"
#include "../../translator/guest/aarch64/abi.h" // the cpu interface os/linux/ is written against
#define HL_GUEST_STAT_SIZE HL_LINUX_STAT_AARCH64_SIZE
#define HL_GUEST_STAT_ENCODE hl_linux_stat_encode_aarch64
#define HL_GUEST_BOUND_STAT hl_linux_stat_aarch64
#include "../../linux_abi/guest_stat.h"
#undef HL_GUEST_BOUND_STAT
#undef HL_GUEST_STAT_ENCODE
#undef HL_GUEST_STAT_SIZE
// Byte size of the guest `struct stat` stat.c writes -- the shared stat syscalls (os/linux/syscall/
// fs.c cases 79/80) validate exactly this many guest bytes before filling the buffer (EFAULT guard).
#define GUEST_LINUX_STAT_BYTES 128

// container/ns config state + parsers (early globals)
#include "../../linux_abi/container/state.c"

static int hl_target_credentials_publish(struct cpu *c) {
    hl_syscall_cpu_aarch64 snapshot;
    hl_syscall_trap_result result;
    if (g_syscall_trap_dispatch == NULL) return 1;
    cred_init();
    memset(&snapshot, 0, sizeof(snapshot));
    snapshot.abi = HL_SYSCALL_TRAP_ABI;
    snapshot.size = sizeof(snapshot);
    snapshot.x[0] = (uint32_t)g_ruid;
    snapshot.x[1] = (uint32_t)g_euid;
    snapshot.x[2] = (uint32_t)g_suid;
    snapshot.x[3] = (uint32_t)newfile_uid();
    snapshot.x[4] = (uint32_t)g_rgid;
    snapshot.x[5] = (uint32_t)g_egid;
    snapshot.x[6] = (uint32_t)g_sgid;
    snapshot.x[7] = (uint32_t)newfile_gid();
    snapshot.x[8] = HL_TASK_EVENT_CREDENTIALS_CHANGED;
    snapshot.task = (uint64_t)(c->tid ? c->tid : container_pid());
    memset(&result, 0, sizeof(result));
    result.abi = HL_SYSCALL_TRAP_ABI;
    result.size = sizeof(result);
    int32_t status = g_syscall_trap_dispatch(g_syscall_trap_context, HL_GUEST_ISA_AARCH64, &snapshot, &result);
    int accepted = status == 0 && result.abi == HL_SYSCALL_TRAP_ABI && result.size >= sizeof(result) &&
                   (result.outcome == HL_SYSCALL_TRAP_CONTINUE || result.outcome == HL_SYSCALL_TRAP_DECLINED);
    if (accepted) atomic_store_explicit(&g_runtime_credentials_published, 1, memory_order_release);
    return accepted;
}

#include "../../linux_abi/fdcache.h"
#include "../../linux_abi/container/vfs/gmap.h"
#include "../../linux_abi/container/owner.h"

// code cache + block map + chaining
#include "../../translator/cache.c"
#include "../../translator/guest_memory.h"
#include "../../linux_abi/logical_vma.h"

// The translator may not link the Linux ABI (DOCS.md 3), so the engine hands it the ledger lookup the
// instruction fetch needs. The data/non-PIE entries are x86-only; an aarch64 guest never uses them.
static const hl_guest_memory_ops g_guest_memory_ops = {
    .resolve_exec = hl_logical_vma_resolve_exec,
    .exec_span = hl_logical_vma_resolve_exec_span,
    .exec_generation = hl_logical_vma_global_exec_generation,
};

static const hl_host_services *effective_host_services(void) {
    return hl_target_services_effective(&g_target_services);
}

static void emit_crash_diagnostic(const char *message, size_t size) {
    const hl_host_services *host = effective_host_services();
    if (host != NULL && host->log != NULL && host->log->emit != NULL)
        host->log->emit(host->context, HL_LOG_TAG_SIGNAL, message, size);
}

// Host-CPU fork: an AArch64 host takes the same-ISA transliterating JIT below; any other takes interp.c,
// which supplies the same seam by decoding AArch64. Both share struct cpu: it is the checkpoint format.
#include "../../host/cpu.h"
#if defined(HL_HOST_CPU_AARCH64) && !defined(HL_A64_INTERPRETER_SMOKE)
// Keep the unity consumers' compact encoder vocabulary while the assembler itself is an independently
// compiled, explicitly-stateful translator component.
#include "../../translator/host/aarch64/asm.h"

static void emit32(uint32_t instruction) {
    hl_a64_emit32(&g_emit, instruction);
}

static void e_str(int rt, int rn, int off) {
    hl_a64_str(&g_emit, rt, rn, off);
}

static void e_ldr(int rt, int rn, int off) {
    hl_a64_ldr(&g_emit, rt, rn, off);
}

static void e_mov_sp_from(int rn) {
    hl_a64_mov_sp_from(&g_emit, rn);
}

static void e_mov_from_sp(int rd) {
    hl_a64_mov_from_sp(&g_emit, rd);
}

static void e_movz(int rd, uint32_t immediate, int shift) {
    hl_a64_movz(&g_emit, rd, immediate, shift);
}

static void e_movk(int rd, uint32_t immediate, int shift) {
    hl_a64_movk(&g_emit, rd, immediate, shift);
}

static void e_br(int rn) {
    hl_a64_br(&g_emit, rn);
}

static void e_movconst(int rd, uint64_t value) {
    hl_a64_movconst(&g_emit, rd, value);
}

static void e_stp(int rt, int rt2, int rn, int off) {
    hl_a64_stp(&g_emit, rt, rt2, rn, off);
}

static void e_ldp(int rt, int rt2, int rn, int off) {
    hl_a64_ldp(&g_emit, rt, rt2, rn, off);
}

static void e_addi(int rd, int rn, unsigned immediate) {
    hl_a64_addi(&g_emit, rd, rn, immediate);
}

static void e_addlsl4(int rd, int rn, int rm) {
    hl_a64_addlsl4(&g_emit, rd, rn, rm);
}

static void e_addlsl3(int rd, int rn, int rm) {
    hl_a64_addlsl3(&g_emit, rd, rn, rm);
}

static void e_movr(int rd, int rm) {
    hl_a64_movr(&g_emit, rd, rm);
}

static void e_subi(int rd, int rn, unsigned immediate) {
    hl_a64_subi(&g_emit, rd, rn, immediate);
}

static void e_hret(void) {
    hl_a64_ret(&g_emit);
}

static void e_adrp_add(int rd, uint64_t target) {
    hl_a64_adrp_add(&g_emit, rd, target);
}

static void e_load_cpu(int reg) {
    hl_a64_load_cpu(&g_emit, reg, (uintptr_t)g_cpu_key);
}

static void e_stur(int rt, int rn, int immediate) {
    hl_a64_stur(&g_emit, rt, rn, immediate);
}

static void e_ldur(int rt, int rn, int immediate) {
    hl_a64_ldur(&g_emit, rt, rn, immediate);
}

static void e_stp_q(int rt, int rt2, int rn, int off) {
    hl_a64_stp_q(&g_emit, rt, rt2, rn, off);
}

static void e_ldp_q(int rt, int rt2, int rn, int off) {
    hl_a64_ldp_q(&g_emit, rt, rt2, rn, off);
}

// persistent cross-process translated-code cache (recorded emitters used by stubs.c/translate.c;
// load/save/relocate). MUST precede stubs.c + translate.c (they call the recorded emitters).
#include "../../translator/guest/aarch64/cache.c"
// engine block-ABI stubs: prologue/spill + IBTC/IC + exit/chain trampolines (built on the assembler above)
#include "../../translator/guest/aarch64/stubs.c"
// transliterate + mangle + §B + LSE + depth-gate
#include "../../translator/guest/aarch64/translate.c"
#else
#include "../../translator/guest/aarch64/interp.c"
#endif
// clone/futex/threads (declares run_guest)
#include "../../linux_abi/thread.c"
// The guest-visible signal trampoline is stored in each AArch64 frame.  A
// normal handler return reaches this readable instruction pair; the
// dispatcher recognizes it before translation and performs rt_sigreturn.
#define G_IS_SIGNAL_RETURN(c)                                                                                           \
    (G_PC(c) == SIGRETURN_PC ||                                                                                         \
     ((c)->sig_depth > 0 && G_PC(c) == (c)->sig_frame_sp[(c)->sig_depth - 1] + UINT64_C(4608)))

// signal delivery
#include "../../linux_abi/signal.c"

static uint64_t signal_canonicalize_pc(void *context, uint64_t pc) {
    (void)context;
    return pcrel_base(pc);
}

static int signal_cache_contains(void *context, uint64_t pc) {
    (void)context;
    return jit_pc_in_retained_cache(pc);
}

static void build_signal_frame(struct cpu *c, int sig, int synchronous) {
    hl_aarch64_signal_state state = {
        .handler = g_sigact[sig].handler,
        .flags = g_sigact[sig].flags,
        .mask = g_sigact[sig].mask,
        .error = &g_sigerror[sig],
        .code = synchronous ? &c->sync_code : &g_sigcode[sig],
        .value = &g_sigval[sig],
        .address = synchronous ? &c->sync_address : &g_sigaddr[sig],
        .pid = &g_sigpid[sig],
        .uid = &g_siguid[sig],
        .sigreturn_pc = SIGRETURN_PC,
        .trace = g_trace,
        .canonicalize_pc = signal_canonicalize_pc,
        .callback_context = NULL,
    };
    hl_aarch64_signal_build(c, sig, &state);
}

static void do_sigreturn(struct cpu *c) {
    hl_aarch64_signal_restore(c);
}

// Fault capture is per BACKEND: the JIT reconstructs guest state from the host mcontext and refines the host
// PC via the provenance map and cpu->mscratch folds; the interpreter has cpu->pc current. Both return 0 for
// "not a guest fault".
#if defined(HL_HOST_CPU_AARCH64) && !defined(HL_A64_INTERPRETER_SMOKE)
static int sigframe_capture_fault(struct cpu *c, void *native_context) {
    if (!hl_aarch64_signal_capture(c, native_context, signal_cache_contains, NULL)) return 0;
    uint64_t exact_pc;
    uint64_t host_pc = (uint64_t)HL_HOST_UC_PC((ucontext_t *)native_context);
    if (jit_instruction_guest_pc(host_pc, &exact_pc)) {
        c->pc = exact_pc;
        // Folded-fault register reconstruction: when guestbase is on, a synchronous fault on a folded memory
        // instruction lands while the fold's scratch host registers (Sb,T,T2,Tm) hold translator temporaries,
        // not the guest values. hl_aarch64_signal_capture copied those clobbered host regs into c->x[], so the
        // guest handler would see STALE registers. The live guest values were spilled to cpu->mscratch[4..7]
        // (emit_fold_mem, OFF_MSCRATCH+32); replay the emitter's exact slot mapping to restore them.
        uint32_t insn = *(const uint32_t *)(uintptr_t)exact_pc;
        int base = (int)((insn >> 5) & 31);
        if (guestbase_on() && is_foldable_mem(insn) && base != 31) {
            int slots[4], nsl = fold_mem_scratch(insn, slots);
            for (int i = 0; i < nsl; i++)
                c->x[slots[i]] = c->mscratch[4 + i];
        }
    }
    return 1;
}
#else
static int sigframe_capture_fault(struct cpu *c, void *native_context) {
    return interp_signal_capture(c, native_context);
}
#endif

// The JIT returns into block_return to unwind the translated frame; the interpreter siglongjmps to run_block.
#if defined(HL_HOST_CPU_AARCH64) && !defined(HL_A64_INTERPRETER_SMOKE)
static void sigframe_resume_dispatch(struct cpu *c, void *native_context) {
    hl_aarch64_signal_resume(c, native_context, (uintptr_t)block_return);
}
#else
static void sigframe_resume_dispatch(struct cpu *c, void *native_context) {
    interp_signal_resume(c, native_context);
}
#endif
// path jail + overlay + /proc synth
#include "../../linux_abi/container/vfs.c"
#include "../../linux_abi/image.h"

static const void *g_initial_executable_image;
static size_t g_initial_executable_size;
static const void *g_authorized_executable_image;
static size_t g_authorized_executable_size;
static char g_authorized_executable_path[4200];

static int aarch64_image_read(const char *path, hl_linux_image *image) {
    if (g_initial_executable_image != NULL)
        return hl_linux_image_read_bytes(g_initial_executable_image, g_initial_executable_size, image);
    if (g_authorized_executable_image != NULL && path != NULL && g_authorized_executable_path[0]) {
        char canonical[4200];
        if (realpath(path, canonical) != NULL && strcmp(canonical, g_authorized_executable_path) == 0)
            return hl_linux_image_read_bytes(g_authorized_executable_image, g_authorized_executable_size, image);
    }
    if (g_rootfs == NULL) return hl_linux_image_read(effective_host_services(), path, image);
    char guest[4200];
    const char *request = path;
    if (path != NULL && path[0] == '/') {
        int backing = g_rootfs && !strncmp(path, g_rootfs_canon, g_rootfs_canon_len) &&
                      (path[g_rootfs_canon_len] == 0 || path[g_rootfs_canon_len] == '/');
        for (int volume = 0; !backing && volume < g_nvols; ++volume)
            backing = !strncmp(path, g_vols[volume].hcanon, g_vols[volume].hlen) &&
                      (path[g_vols[volume].hlen] == 0 || path[g_vols[volume].hlen] == '/');
        for (int lower = 0; !backing && lower < g_nlower; ++lower) {
            if (!strncmp(path, g_lower[lower].canon, g_lower[lower].clen) &&
                (path[g_lower[lower].clen] == 0 || path[g_lower[lower].clen] == '/')) {
                const char *suffix = path + g_lower[lower].clen;
                snprintf(guest, sizeof guest, "%s", suffix[0] ? suffix : "/");
                request = guest;
                backing = 2;
            }
        }
        if (backing == 1) {
            guest_from_host_raw(path, guest, sizeof guest);
            request = guest;
        }
    }
    if (request != NULL && request[0] == '/' && (g_rootfs != NULL || jail_match(request) >= 0)) {
        if (g_nlower) {
            char backing[4200];
            /* ELF loading follows the final executable symlink, just like execve.
               overlay_lookup deliberately preserves that link for lstat/readlink,
               so using it here made ordinary image links such as /bin/python fail
               the O_NOFOLLOW open below.  Resolve the complete merged-view chain;
               the returned path is still confined to the selected overlay layer. */
            if (!overlay_resolve(request, backing, sizeof backing, 0)) return -1;
            int descriptor = open(backing, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
            int result = descriptor < 0 ? -1 : hl_linux_image_read_fd(descriptor, image);
            if (descriptor >= 0) close(descriptor);
            return result;
        }
        char final[512];
        int directory = jail_at(-100, request, final, sizeof final, 0);
        if (directory < 0) return -1;
        int descriptor = openat(directory, final, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
        int result = descriptor < 0 ? -1 : hl_linux_image_read_fd(descriptor, image);
        if (descriptor >= 0) close(descriptor);
        close(directory);
        return result;
    }
    return hl_linux_image_read(effective_host_services(), request, image);
}

// termios + NET-ns loopback
#include "../../linux_abi/container/netns.c"
// ELF fwd-decls + FS-metadata cache
struct main_placement;
static void load_elf(const char *path, struct loaded *out, const struct main_placement *placement,
                     const hl_linux_image *pinned);
static int elf_interp(const char *path, char *out, size_t n, const hl_linux_image *pinned);
static uint64_t build_stack(int argc, char **argv, struct loaded *lm, uint64_t at_base);
// the syscall layer (service())
#include "../../linux_abi/syscall/dispatch.c"
// untrusted-guest isolation: SPSC ring + sentry split (g_untrusted; OFF by default)
#include "../../linux_abi/sentry.c"
// native checkpoint/restore control seam: the shared dispatcher (engine/dispatch.c) polls G_CKPT_POLL at
// its per-block safepoint. Declared here (before the include) so only the aarch64 TU wires it; ckpt_poll is
// DEFINED in os/linux/checkpoint.c below (a forward-declared static call is legal).
static void ckpt_poll(struct cpu *c);
#define G_CKPT_POLL(c) ckpt_poll(c)
#define G_CKPT_ARCH 2
#define G_CKPT_CPU_SANITIZE(c)                                                                                         \
    do {                                                                                                               \
        (c)->ic_site = 0;                                                                                              \
        G_SOFT_STATE_RESET(c);                                                                                         \
    } while (0)
// checkpoint.c's restore driver (included below) rebuilds the container from these, defined later in this TU.
static int container_init(const char *rootfs);
static int engine_global_init(void);
// host trampoline + run_guest
#include "../dispatch.c"
// ELF loader + initial stack
#include "../../linux_abi/elf.c"
// native checkpoint/restore (multi-process tree): dump/restore guest RAM + cpu + path-backed fds + pty
#include "../../linux_abi/checkpoint.c"

// Final-product bridge: read the serialized HL config file and enter this target's Linux guest.

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
    // These are the JIT's scratch and link registers -- which trampoline was mid-flight -- and mean nothing
    // under another register file. Keep the column layout; they read zero.
    uint64_t hpc = u ? (uint64_t)HL_HOST_UC_PC(u) : 0;
#if defined(HL_HOST_HAS_A64_CONTEXT)
    uint64_t hx0 = u ? hl_host_uc_a64_gpr_get(u, 0) : 0;
    uint64_t hx1 = u ? hl_host_uc_a64_gpr_get(u, 1) : 0;
    uint64_t hx9 = u ? hl_host_uc_a64_gpr_get(u, 9) : 0;
    uint64_t hx10 = u ? hl_host_uc_a64_gpr_get(u, 10) : 0;
    uint64_t hx16 = u ? hl_host_uc_a64_gpr_get(u, 16) : 0;
    uint64_t hx17 = u ? hl_host_uc_a64_gpr_get(u, 17) : 0;
    uint64_t hx30 = u ? hl_host_uc_a64_gpr_get(u, 30) : 0;
#else
    uint64_t hx0 = 0, hx1 = 0, hx9 = 0, hx10 = 0, hx16 = 0, hx17 = 0, hx30 = 0;
#endif
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
#if defined(HL_HOST_HAS_A64_CONTEXT)
    if (u) {
        for (int r = 2; r <= 8; r++)
            bp = diag_reg(b, bp, r, hl_host_uc_a64_gpr_get(u, (unsigned)r));
        for (int r = 11; r <= 15; r++)
            bp = diag_reg(b, bp, r, hl_host_uc_a64_gpr_get(u, (unsigned)r));
        for (int r = 18; r <= 27; r++)
            bp = diag_reg(b, bp, r, hl_host_uc_a64_gpr_get(u, (unsigned)r));
    }
#endif
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
    emit_crash_diagnostic(b, (size_t)bp + 1u);
    _exit(139);
}

static void diag_hx8(char *b, uint32_t v) {
    for (int i = 0; i < 8; i++) {
        int d = (v >> ((7 - i) * 4)) & 0xf;
        b[i] = d < 10 ? '0' + d : 'a' + d - 10;
    }
}

#if defined(__APPLE__)
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
    // (guest/aarch64/cpu.h), so ss->__x[28] is the faulting thread's struct cpu whenever the fault is in the
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
                      : (msg.exception == EXC_BREAKPOINT)    ? SIGTRAP // a guest `brk`
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
        uint64_t off = 0;
        const char *sn = "?";
#if !defined(_WIN32)
        Dl_info info;
        if (dladdr((void *)st.__pc, &info)) {
            off = st.__pc - (uint64_t)info.dli_fbase;
            if (info.dli_sname) sn = info.dli_sname;
        }
#endif
        memcpy(b + 106, " off=0x", 7);
        diag_hx(b + 113, off);
        b[129] = ' ';
        int sl = 0;
        while (sn[sl] && sl < 40) {
            b[130 + sl] = sn[sl];
            sl++;
        }
        hl_host_region region = {0};
        int region_found = hl_host_region_query(st.__pc, &region);
        // Append the GUEST pc (x28 is the reserved CPUREG -> struct cpu*), so a guest `brk`/fault maps to a
        // guest instruction, not just the host JIT pc. Guarded: x28 may not be a cpu ptr outside the cache.
        int bp = 130 + sl;
        memcpy(b + bp, " rkr=0x", 7);
        bp += 7;
        diag_hx8(b + bp, region_found ? 0 : 1);
        bp += 8;
        if (region_found) {
            memcpy(b + bp, " rbase=0x", 9);
            bp += 9;
            diag_hx(b + bp, region.address);
            bp += 16;
            memcpy(b + bp, " rsz=0x", 7);
            bp += 7;
            diag_hx(b + bp, region.size);
            bp += 16;
            memcpy(b + bp, " prot=0x", 8);
            bp += 8;
            diag_hx8(b + bp, region.protection);
            bp += 8;
            memcpy(b + bp, " rmap=0x", 8);
            bp += 8;
            diag_hx8(b + bp, (st.__pc >= region.address && st.__pc - region.address < region.size) ? 1 : 0);
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
        for (int r = 2; r <= 8; r++)
            bp = diag_reg(b, bp, r, st.__x[r]);
        for (int r = 11; r <= 15; r++)
            bp = diag_reg(b, bp, r, st.__x[r]);
        for (int r = 18; r <= 27; r++)
            bp = diag_reg(b, bp, r, st.__x[r]);
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
        emit_crash_diagnostic(b, (size_t)bp + 1u);
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
#endif

// Fork-server seam: the original guest entry inlined (1)
// container init, (2) engine init (signal handlers + pthread key + code-cache arena + env flags), and
// (3) per-launch load+run. The resident engine server pays (1)+(2) once and shares them COW with
// every forked worker, so those phases are factored into container_init()/engine_global_init().
// engine_global_init() is idempotent (g_engine_inited) so the standalone path is unchanged: standalone
// hl_run_linux_guest() composes container_init -> engine_global_init -> load_program -> run_loaded in the exact
// original order, with the identical operations in each phase.
static int g_engine_inited;

static int container_init(const char *rootfs) {
    g_rootfs_mode = rootfs != NULL && rootfs[0] != 0;
#if defined(__APPLE__)
    hl_linux_dns_prepare();
#endif
    hl_gmap_bind_limits(&g_limits);
    // PID ns: only containers (rootfs) get PID 1
    if (rootfs) g_init_hostpid = getpid();
    // cross-engine-process cgroup accounting: a FRESH shared slot table for THIS container init, inherited
    // by every guest fork (see state.c). Per-container so sibling forkserver workers never share a total.
    if (rootfs) acct_container_reset(effective_host_services());
    {
        const char *h = hl_option_get("HL_HOSTNAME");
        // hl-engine -> jit config
        if (h && !g_hostname[0]) { strncpy(g_hostname, h, 64); }
        const char *m = hl_option_get("HL_MEM_MAX");
        if (m && !g_mem_max) g_mem_max = parse_size(m);
        const char *p = hl_option_get("HL_PIDS_MAX");
        if (p && !g_pids_max) g_pids_max = hl_parse_id("HL_PIDS_MAX", p);
        container_read_resource_env(); // Docker CPU, read-only-root, and ulimit values from centralized HL options.
        const char *pub = hl_option_get("HL_PUBLISH");
        if (pub && hl_linux_ports_count(&g_ports) == 0) parse_publish(pub);
        const char *low = hl_option_get("HL_LOWER");
        if (low && !g_nlower) {
            char tb[8192];
            // HL_LOWER is a newline-record option, so host paths may contain ':'.
            snprintf(tb, sizeof tb, "%s", low);
            char *sv = NULL;
            for (char *t = strtok_r(tb, "\n", &sv); t; t = strtok_r(NULL, "\n", &sv))
                add_lower(t);
        }
        // Private-loopback netns: derive g_netns from the HL_NETNS key (set per-container by the
        // daemon; a `docker exec` reuses the target container's key so the exec shares its 127.0.0.1).
        // When HL_NETNS is unset (a bare engine launch), mint a per-process key and
        // export it — so loopback isolation is ALWAYS on. Otherwise g_netns stayed empty, lo_on() was
        // false, and 127.0.0.1 fell through to the REAL host TCP stack shared by every concurrent guest,
        // so two containers' 127.0.0.1:PORT collided (nc-loopback intermittently reached another
        // container's published listener). Mirrors linux_x86_64.c.
        if (!g_netns[0] && hl_option_get("HL_NET_HOST") == NULL) {
            const char *nn = hl_option_get("HL_NETNS");
            char key[40];
            if (nn && nn[0])
                snprintf(key, sizeof key, "%.39s", nn);
            else
                snprintf(key, sizeof key, "%d", (int)getpid());
            namespace_key_set(key);
            snprintf(g_netns, sizeof g_netns, "/tmp/.hl-net-%.40s", key);
            // Export the minted key so children/exec + abstract-AF_UNIX/IPC/bridge share this
            // container's namespace (hl_option_get("HL_NETNS")); a daemon-supplied key is already in the env.
            if (hl_target_services_make_directory(&g_target_services, g_netns, 0700) == 0 && !(nn && nn[0]))
                hl_option_set("HL_NETNS", key, 1);
        } else if (g_netns[0] && hl_target_services_make_directory(&g_target_services, g_netns, 0700) != 0)
            return -1;
        const char *eu = hl_option_get("HL_UID");
        if (eu) g_uid = hl_parse_id("HL_UID", eu);
        const char *eg = hl_option_get("HL_GID");
        if (eg) g_gid = hl_parse_id("HL_GID", eg);
        // USER ns (process.user)
    }
    if (rootfs && rootfs[0]) {
        const char *owner_lowers[8];
        for (int i = 0; i < g_nlower; ++i)
            owner_lowers[i] = g_lower[i].canon;
        g_rootfs = (char *)rootfs;
        if (root_handle_bind(g_rootfs) != 0 || root_native_require() != 0) return -1;
        if (hl_owner_seed(g_rootfs, hl_option_get("HL_FILE_OWNERS"), owner_lowers, (size_t)g_nlower) != 0) return -1;
        container_populate_dev();        // /dev/{fd,stdin,stdout,stderr,ptmx,pts,shm,console,...} the unpacker stripped
        container_populate_machine_id(); // /etc/machine-id agreeing with boot_id (if image ships none)
        if (g_uid < 0) g_uid = 0;
        // Container default: run as root (0), unless HL_UID or the typed uid field is set.
        if (g_gid < 0) g_gid = 0;
        cred_reset_initial();
        // Docker -w / initial working directory: start the guest in HL_CWD (must be reachable inside the
        // container -- typically a bind-mounted volume). confine() normalizes + clamps it to the rootfs.
        const char *icwd = hl_option_get("HL_CWD");
        if (icwd && icwd[0]) confine(icwd, g_cwd, sizeof g_cwd);
    }
    if (!rootfs && (root_handle_bind("/") != 0 || hl_owner_seed("/", NULL, NULL, 0) != 0)) return -1;
    // bind-mount volumes: "[ro:]guestpath:hostdir,..." -- delegate to add_vol() (the shared vfs.c parser)
    // so the optional `ro:` read-only marker is handled in ONE place for both engines. Ingested regardless
    // of whether a rootfs is set (matching linux_x86_64.c): add_vol opens the HOST dir directly and the
    // registration is what surfaces the bind in /proc/self/mountinfo + /proc/mounts.
    const char *vspec = hl_option_get("HL_VOLUMES");
    if (vspec && vspec[0]) {
        char *tmp = strdup(vspec);
        if (tmp == NULL) return -1;
        char *sv = NULL;
        for (char *t = strtok_r(tmp, ",", &sv); t; t = strtok_r(NULL, ",", &sv))
            add_vol(t);
        free(tmp);
    }
    if (name_binds_parse(hl_option_get("HL_NAME_BINDS")) != 0) return -1;
    // derive the run user's supplementary group set from the image rootfs (runc additionalGids), after
    // g_uid/g_gid + the overlay lowers are resolved, so getgroups(2) and /proc/self/status Groups: report it.
    if (g_rootfs) container_parse_groups();
    return 0;
}

// idempotent engine init (fault handlers + pthread key + code-cache arena + env-flag reads).
// Returns 0 on success, nonzero exit code on failure. First call wins; later calls are no-ops
// (g_engine_inited), so the resident fork-server parent pays this once and the standalone path runs it
// exactly as before.
static int guest_fetch_direct_valid(uint64_t address, size_t length) {
    return host_range_mapped((uintptr_t)address, length);
}

static int engine_global_init(void) {
    hl_guest_memory_bind(&g_guest_memory_ops);
    hl_guest_fetch_set_direct_validator(guest_fetch_direct_valid);
    if (hl_target_services_bind(&g_target_services) != 0) return 1;
    if (g_engine_inited) return 0;
    // Serve a non-PIE ET_EXEC's absolute data references at their biased host address. Faults not
    // belonging to this compatibility path are re-raised with the default action.
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = nonpie_guard;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL);
    if (pthread_key_create(&g_cpu_key, NULL) != 0) {
        perror("pthread_key_create");
        return 1;
    }
    if (g_cpu_key >= 4096) {
        fprintf(stderr, "[jit] TSD key %u too large for inline ldr\n", (unsigned)g_cpu_key);
        return 1;
    }

    // Code cache ownership belongs to the macOS host-service backend. NODUALMAP retains the single-MAP_JIT
    // compatibility mode; the default is a stable RW/RX alias pair repaired by the backend after fork.
    if (jit_cache_init() != 0) {
        g_engine_result_status = hl_fatal_status(&g_jit_fatal);
        return 70;
    }
    {
        hl_fdcache_binding binding = {hl_target_services_bound(&g_target_services),
                                      &g_vfs_namespace,
                                      &g_nvols,
                                      g_rootfs_canon,
                                      &g_rootfs_canon_len,
                                      g_fdpath,
                                      1024,
                                      &g_threaded,
                                      hl_option_get("HL_FSGEN_FILE")};
        if (hl_fdcache_bind(&binding) != 0) {
            fprintf(stderr, "hl-engine: unable to initialize filesystem caches\n");
            return 1;
        }
    }

    g_trace = 0;
    g_systrace = 0;
    g_dbg_nochain = 0;
    g_dbg_gprdump = 0;
    g_prof = hl_option_get("HL_C_DIAGNOSTICS") != NULL;
    g_service_ns = 0;
    g_prof_soft_hull_sampled = g_prof_soft_cached_sampled = g_prof_soft_sites_sampled = 0;
    g_prof_soft_miss = g_prof_soft_span = 0;
    g_prof_soft_bounce_prepare = g_prof_soft_bounce_commit = 0;
    g_prof_smc_queued = g_prof_smc_commit = 0;
    g_no_stw_reclaim = 0;
    g_steal1617 = 1;
    g_noibslim = 0;
    g_fwdskip = 8;
    g_guestfold = 1;
    g_mtibtc = 1;
    // Untrusted-guest isolation (the sentry process-split). OFF by default -> trusted path unchanged.
    g_untrusted = hl_option_get("HL_UNTRUSTED") != NULL;
    g_sentry_sandbox = hl_option_get("HL_SANDBOX") != NULL;
    // pcache_poison_check runs AFTER the codegen-mode flags above so it can refuse to persist an arena
    // that a non-default mode (PROF) baked unrecorded host pointers
    // into. The cache mode is read per guest invocation so a fork-server runner honors HL_PCACHE.
    pcache_poison_check();
    // ptrace tracer/tracee coordination arena -- mmap the shared region ONCE here, BEFORE any guest
    // fork, so every descendant guest process inherits the same physical pages. Inert until a guest ptraces.
    ptrace_arena_init();
    // POSIX record locks and BSD flock ownership share a process-tree broker. Embedded builds skip the
    // standalone constructor, so initialize it explicitly before any guest fork can inherit this engine.
    if (poslk_init() != 0) return 70;
    // Arm the generation-counter checkpoint control channel when HL_CHECKPOINT is set.
    // Runs in every process (init + forked children) so the whole tree is checkpointable.
    if (ckpt_control_init() != 0) return 70;
    g_engine_inited = 1;
    return 0;
}

// load main program + (optional) interp, recording the load base/entry/at_base into *lm/*li.
// Used both by the standalone path and by the fork-server's parent preload (so the COW-inherited image
// is byte-identical and the warm worker re-runs from the same entry at the same base). The gb/pb/ib
// buffers are static because g_exe_path = prog points into gb and must outlive this call.
static const char *load_program(const char *prog, struct loaded *lm, struct loaded *li, uint64_t *jump,
                                uint64_t *at_base, int *have_interp, const hl_engine_main_image_plan *image_plan) {
    // cache id keys the INVOKED name (argv[0] pre-resolution), exactly as before this refactor.
    const char *argv0 = prog;
    static char gb[1024];
    prog = find_in_path(prog, gb, sizeof gb); // bare "sh" (docker) -> "/bin/sh" via the container PATH
    g_exe_path = prog;
    // /proc/self/exe must be the ABSOLUTE, CANONICAL guest path of the loaded image: a RELATIVE guest
    // invocation ("./x" from a harness) or an entry symlink (/bin/sh->busybox) otherwise leaks into the
    // link value, and glibc static-pie ASSERTS on it at startup ("dl-origin.c: linkval[0]=='/'"). Done
    // Here, not only in the guest entry, so parent preload also hands every COW warm worker a canonical
    // cold launch). Static: the value must outlive this call, like gb above. Mirrors linux_x86_64.c.
    static char bootexe[4200];
    exe_canon(prog, bootexe, sizeof bootexe);
    g_exe_path = bootexe;
    static char pb[4200];
    const char *prog_host =
        // resolve through the overlay (upper, then lowers) + follow the entry symlink (/bin/sh->busybox)
        xresolve_overlay(prog, pb, sizeof pb);
    // Authorize the launched image for the bare-mode execve gate (proc.c) and the by-path image reader.
    // This must be set even when no executable image was embedded (the macOS embedding/bridge path launches
    // the guest purely by path): otherwise a guest re-exec of /proc/self/exe fails the authorized-target
    // check with ENOENT. In the normal production path g_initial_executable_image is always set, so only the
    // embedded by-path case changes here.
    if (!g_authorized_executable_path[0]) {
        if (realpath(prog_host, g_authorized_executable_path) == NULL)
            snprintf(g_authorized_executable_path, sizeof g_authorized_executable_path, "%s", prog_host);
    }
    // Persistent translations and checkpoint images both record guest addresses.  Give both modes the
    // deterministic layout already used by the x86 target so a capture can be restored in a fresh process.
    if (g_pcache || hl_option_get("HL_CHECKPOINT")) g_force_base = PC_IMG_BASE;
    struct main_placement main_placement;
    const struct main_placement *placement = NULL;
    if (image_plan != NULL) {
        if (main_placement_from_plan(image_plan, &main_placement) != 0) {
            fprintf(stderr, "hl-engine: invalid Rust main image placement plan\n");
            exit(1);
        }
        placement = &main_placement;
    }
    load_elf(prog_host, lm, placement, NULL);

    // Dynamic: load the PT_INTERP (ld.so) and enter THERE; it loads libs + relocates.
    *jump = lm->entry;
    *at_base = 0;
    *have_interp = 0;
    const char *interp_host = NULL;
    char interp[256];
    int has_interp = elf_interp(prog_host, interp, sizeof interp, NULL) == 0;
    g_initial_executable_image = NULL;
    g_initial_executable_size = 0;
    if (has_interp) {
        static char ib[4200];
        // follow+confine ld.so symlink (through the overlay)
        interp_host = xresolve_overlay(interp, ib, sizeof ib);
        if (g_pcache || hl_option_get("HL_CHECKPOINT")) g_force_base = PC_INTERP_BASE;
        load_elf(interp_host, li, NULL, NULL);
        *jump = li->entry;
        *at_base = li->base;
        *have_interp = 1;
    }
    // key the cache by the identity of the guest binary + interp (+ the invoked name).
    if (g_pcache) g_pc_binid = pcache_make_id(prog_host, interp_host, argv0);
    return prog;
}

// fresh per-launch guest run from a loaded image. Allocates a private heap + a guest stack +
// cpu and runs from `jump`. Shared by standalone/cold and fork-server warm-worker paths (which
// restores a pristine COW image first, then calls this against the parent-preloaded base). Body is the
// preserve the same execution tail.
static int run_loaded(int argc, char *const argv[], struct loaded *lm, uint64_t jump, uint64_t at_base) {
    // checkpoint/restore: place the brk heap in the deterministic high arena (0 hint => normal NULL placement)
    uint64_t heap;
    if (hl_gmap_map_anonymous(hl_linux_snapshot_reserve(&g_ckpt_snapshot, 256u << 20), 256u << 20,
                              HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE, HL_HOST_MEMORY_PRIVATE,
                              &heap) != HL_STATUS_OK)
        return 70;
    brk_lo = brk_cur = heap;
    brk_hi = brk_lo + (256u << 20);

    struct cpu c;
    memset(&c, 0, sizeof c);
    // guest argv = prog + its args
    c.sp = build_stack(argc, (char **)argv, lm, at_base);
    c.pc = jump;

    proc_reg_publish(g_exe_path, argc, argv); // publish this process into the /proc table
    if (g_untrusted) sentry_init();           // fork the host-authority sentry + (optionally) confine the worker
    thread_process_owner_register(&c);
    run_guest(&c);
    c.exit_code = thread_process_owner_wait(&c, c.exit_code);
    if (g_untrusted) sentry_shutdown(); // signal quit + waitpid (reap, no orphan)
    if (g_prof) {
        char profile[160];
        int profile_size = snprintf(profile, sizeof profile, "[prof] dispatcher crossings=%llu translations=%llu\n",
                                    (unsigned long long)g_dispatch_profile.crossings,
                                    (unsigned long long)g_dispatch_profile.translations);
        if (profile_size > 0) {
            size_t bounded = (size_t)profile_size < sizeof profile ? (size_t)profile_size : sizeof profile - 1u;
            (void)hl_linux_write(g_linux_box, STDERR_FILENO, profile, bounded);
        }
    }
    return c.exit_code;
}

// Rebuild the checkpointed process tree selected by the engine restore option. Guest memory for the init is rebuilt
// FIRST -- before container_init/engine_global_init allocate anything -- inside ckpt_restore_tree.
static int hl_restore_checkpoint(const char *rootfs) {
    g_pcache = hl_option_get("HL_PCACHE") != NULL;
    return ckpt_restore_tree(rootfs);
}

int hl_run_linux_guest(const hl_host_services *host, hl_linux_abi *box, const char *rootfs, hl_host_handle executable,
                       const void *executable_image, size_t executable_size,
                       const hl_engine_main_image_plan *image_plan, uint32_t argument_count, char *const argv[]) {
    int argc;
    (void)executable;
    g_initial_executable_image = executable_image;
    g_initial_executable_size = executable_size;
    g_authorized_executable_image = executable_image;
    g_authorized_executable_size = executable_size;
    g_engine_result_status = HL_STATUS_OK;
    if (argument_count > (uint32_t)INT_MAX) return 2;
    argc = (int)argument_count;
    hl_target_services_inject(&g_target_services, host);
    hl_gmap_bind_host(host);
    futex_table_init(host);
    seq_ref_arena_init(host);
    eventfd_count_init(host);
    fdvis_init(host);
    ts_init(host);
    g_linux_box = box;
    jit_guest_bus_bind(hl_linux_bus_fault, hl_linux_bus_active(), hl_linux_bus_generation());
    hl_linux_bus_set_change_callback(jit_guest_bus_changed, NULL);
    hl_linux_file_events_set_callback(bound_mapping_journal_apply, NULL);
    hl_linux_bus_set_transition_callbacks(jit_guest_bus_transition_begin, jit_guest_bus_transition_end, NULL);
    g_host_launch_monotonic_ns = 0;
    if (host != NULL) {
        hl_host_result now;
        if (hl_host_services_validate(host, HL_HOST_CAP_CLOCK) != HL_STATUS_OK) {
            fprintf(stderr, "hl-engine: restore prerequisite host clock validation failed\n");
            return hl_vfs_cursor_state_finish(70);
        }
        now = host->clock->monotonic_ns(host->context);
        if (now.status != HL_STATUS_OK) {
            fprintf(stderr, "hl-engine: restore prerequisite monotonic clock read failed\n");
            return hl_vfs_cursor_state_finish(70);
        }
        g_host_launch_monotonic_ns = now.value;
    }
    if (bound_shadow_activate() != 0) {
        fprintf(stderr, "hl-engine: restore prerequisite shadow descriptor activation failed\n");
        return hl_vfs_cursor_state_finish(70);
    }
    // Resume a previously checkpointed workspace instead of launching a program (the embedding host sets this on
    // window reopen; the container config/env is otherwise identical to the original launch).
    const char *rdir = hl_option_get("HL_RESTORE");
    if (rdir != NULL) return hl_vfs_cursor_state_finish(hl_restore_checkpoint(rootfs));
    if (argc < 1 || !argv || !argv[0]) return hl_vfs_cursor_state_finish(2);
    // Persistent cross-process translated-code cache. Opt in with HL_PCACHE=1.
    // Read per invocation so a fork-server cold runner honors its typed launch configuration.
    g_pcache = hl_option_get("HL_PCACHE") != NULL;
    g_coldprof = 0;
    if (container_init(rootfs) != 0) return hl_vfs_cursor_state_finish(70);
    int irc = engine_global_init();
    if (irc) return hl_vfs_cursor_state_finish(irc);
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
        fprintf(stderr, "hl-engine: too many nested #! interpreters (ELOOP): %s\n", prog);
        return hl_vfs_cursor_state_finish(40); // ELOOP
    }
    if (sb_new != sb_argc) { // a shebang chain resolved -> run the final interpreter, not the script
        argc = sb_new;
        argv = (char *const *)sb_argv;
        // load_program below re-resolves argv[0] and re-sets g_exe_path to the binary actually loaded
        // (matches /proc/self/exe for a script exec).
    }
    // /proc/self/exe canonicalization now happens inside load_program (below), so it also covers the
    // fork-server parent preload path. load_program re-resolves
    // argv[0] and sets g_exe_path to the canonical absolute path of the binary actually loaded, matching
    // /proc/self/exe for a script exec.
    struct loaded lm, li;
    uint64_t jump, at_base;
    int have_interp;
    load_program(argv[0], &lm, &li, &jump, &at_base, &have_interp, image_plan);
    // try to restore the arena from the persistent cache.
    if (g_pcache) {
        g_pc_entry = jump;
        int hit = pcache_load(jump); // graceful MISS on any stale/corrupt/truncated cache -> translate fresh
        if (g_coldprof) fprintf(stderr, "[pcache] %s reloc=%d\n", hit ? "HIT" : "MISS", g_nreloc);
        if (hl_fatal_status(&g_jit_fatal) != HL_STATUS_OK) {
            g_engine_result_status = hl_fatal_status(&g_jit_fatal);
            pcache_directory_close();
            return hl_vfs_cursor_state_finish(70);
        }
    }
    int ec = run_loaded(argc, argv, &lm, jump, at_base);
    if (hl_fatal_status(&g_jit_fatal) == HL_STATUS_OK)
        pcache_save(); // persist on a cold miss (guest exit via case 93 returns here; case 94 saves + _exit)
    if (hl_fatal_status(&g_jit_fatal) != HL_STATUS_OK) {
        g_engine_result_status = hl_fatal_status(&g_jit_fatal);
        ec = 70;
    }
    pcache_directory_close();
    return hl_vfs_cursor_state_finish(ec);
}

// resident engine fork server (server/client/worker), shared with the x86-64 target through the
// container-init/engine-init/load/run seam above. aarch64 has no g_loadbase and its container model never
// chdir()s the engine into the rootfs, so those knobs stay default no-ops; the pristine-image restore
// around the guest image's W^X is now the shared default too (fork.c, fsrv_restore_prep/done).
// Bind the same per-guest host-service tables a cold hl_run_linux_guest() would, so the fork-server prewarm
// parent (which runs guests via run_loaded()) allocates them once and every warm COW worker inherits them.
#define FSRV_GUEST_HOST_INIT()                                                                                         \
    do {                                                                                                               \
        const hl_host_services *fsrv_host_ = hl_target_services_effective(&g_target_services);                         \
        futex_table_init(fsrv_host_);                                                                                  \
        seq_ref_arena_init(fsrv_host_);                                                                                \
        eventfd_count_init(fsrv_host_);                                                                                \
        fdvis_init(fsrv_host_);                                                                                        \
        ts_init(fsrv_host_);                                                                                           \
    } while (0)
#include "../../linux_abi/fork.c"

void hl_target_runtime_init(void) {
    install_sync_fault_guards();
    poslk_init();
    ipc_init();
}

uint64_t hl_run_linux_guest_translations(void) {
    return g_dispatch_profile.translations;
}
