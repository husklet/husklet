#include "namespace.h"
#include "../executable_authority.h"
#include "../../translator/digest.h"
#include "../checkpoint_channel.c"
#include "../bus.h"
#include "../../linux_abi/dns.h"

#if defined(HL_NATIVE_TEST_HOOKS)
static int g_test_direct_store_guard_emissions;
static int g_test_store_preflight_active;
static int g_test_store_preflight_protected;
static size_t g_test_store_preflight_prefix;
static int g_test_store_preflight_calls;
#endif

// hl/core/target -- x86-64 Linux guest target composition.
//
// This unity translation unit wires the x86-64 translator frontend to the shared Linux ABI,
// container, host-service, and engine layers. Architecture-specific translation remains under
// translator/guest/x86_64; Linux executable loading and process construction belong to linux_abi.

// jit86.c — an x86-64-guest JIT (x86-64 -> ARM64) for Linux guests on macOS/arm64.
//
// Sibling of runtime/jit/jit.c (which is aarch64->aarch64). See DESIGN.md for the
// full "what breaks / what doesn't" rationale. Short version:
//
//   * The ISA-AGNOSTIC scaffolding (code cache, guest-PC->host-code map, direct-
//     branch chaining, the run_block/block_return trampolines, the Linux->macOS
//     syscall bodies, the Linux ELF loader, rootfs path rewriting) is COPIED+ADAPTED from
//     jit.c. We can't refactor jit.c (it's under active dev), so we duplicate.
//   * The FRONT-END is new: an x86-64 decoder + per-opcode ARM64 codegen, replacing
//     jit.c's "copy the instruction verbatim" core (which only works same-arch).
//
// Register model (the win from x86 having only 16 GPRs, see DESIGN.md §4):
//   guest  rax rcx rdx rbx rsp rbp rsi rdi  r8..r15
//   host    x0  x1  x2  x3  x4  x5  x6  x7  x8..x15   (guest reg# == host reg#)
//   cpu ptr : x28 (PINNED for the whole block)   scratch : x16,x17   forbidden: x18
//   flags   : ARM nzcv saved/restored to cpu->nzcv (exact for cmp/test->jcc, §9)
//
// Status: BOOTSTRAP. Implements enough to run a freestanding write+exit guest and a
// growing slice toward simple busybox. Unknown opcodes print their bytes and exit —
// that is the iterative workflow (run -> see unimpl -> add it -> repeat).

#include <limits.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include "../../linux_abi/host_fd.h"   // <fcntl.h> + <unistd.h>, or the descriptor vocabulary where the host has none
#include "../../linux_abi/host_mman.h" // <sys/mman.h>, or the typed VM seam where the host has none
#include <sys/stat.h>
#include <pthread.h>
#include <errno.h>
#include <time.h>
#include <sys/time.h>
#include "../../linux_abi/host_uio.h" // <sys/uio.h>, or the guest iovec layout where the host has none
#include "../../linux_abi/host_socket.h"
#include "../../linux_abi/host_socket.h"
#include "../../linux_abi/host_socket.h"
#include "../../linux_abi/host_socket.h"
#include "../../linux_abi/host_socket.h"
#include "../../linux_abi/host_proc.h"
#include "../../linux_abi/host_wait.h"
#include "../../linux_abi/device.h"
#include "../../linux_abi/host_poll.h" // <poll.h>, or a typed absence where the host has no mixed-handle readiness
#include "../../host/native_compat.h"
#include "../../host/native_context.h"
#include "../../linux_abi/logical_vma.h"
#include "../../linux_abi/host_dirent.h" // <dirent.h>, or the Linux dirent shape where the host structure has no d_type
#include "../../linux_abi/host_system.h" // sysconf/major/minor/arc4random_buf/fork: the residue no single POSIX header owns
#include "../../linux_abi/host_signal.h" // <signal.h>, or the Linux signal vocabulary where the host has no signals
#include "../../linux_abi/host_tty.h"
#include "../../linux_abi/host_tty.h"
#include <stdatomic.h>

#include "hl/engine.h"
#include "hl/linux_abi.h"
#include "hl/syscall_trap.h"
#include "../options.h"
#include "native.h"
#include "services.h"
#include "../bus.h"
#include "../result.h"
#include "../../linux_abi/bus.h"

/* Instance-scoped host seam supplied by hl_engine. CLI launches retain their native-host path with NULL. */
static hl_target_services g_target_services;
#define g_host_services (g_target_services.injected)
#define g_jit_services (g_target_services.bound)
static hl_status g_engine_result_status;
static hl_linux_abi *g_linux_box;

/* The stable runtime trap record is AArch64-only today. Keep the lifecycle
 * seam present for the namespaced x86 backend while its syscalls remain on
 * the retained engine's internal Linux ABI path. */
void hl_target_syscall_trap_install(void *context, hl_syscall_trap_fn dispatch) {
    (void)context;
    (void)dispatch;
}

struct cpu;

static int hl_target_task_event(struct cpu *cpu, uint64_t event, uint64_t value, uint64_t source, uint64_t child) {
    (void)cpu;
    (void)event;
    (void)value;
    (void)source;
    (void)child;
    return 1;
}

static int hl_target_credentials_publish(struct cpu *cpu) {
    (void)cpu;
    return 1;
}

hl_status hl_run_linux_guest_status(void) {
    return g_engine_result_status;
}

static uint64_t g_host_launch_monotonic_ns;
static uint64_t g_emit_next;
static void filemap_refresh_emulated(uint64_t lo, uint64_t hi);
static int jit_guest_soft_activate(void);
static void jit_guest_soft_restore_activate(void);
static void jit_guest_soft_restore_deactivate(void);
static void jit_guest_soft_deactivate(void);
static int jit_guest_soft_active(void);
uint64_t hl_x86_guest_pointer(uint64_t address);

#include "../../translator/guest/x86_64/cpu.h"
#include "../../translator/guest/x86_64/frame.h"
#include "../../translator/guest/x86_64/abi.h" // cpu-interface seam (G_* contract + sysmap + normalize)
// The dispatch seam is per (guest ISA, HOST CPU): dispatch.h patches AArch64 branch encodings.
#include "../../host/cpu.h"
#include "../../translator/guest_memory.h"
#if defined(HL_HOST_CPU_AARCH64)
#include "../../translator/guest/x86_64/smc/index.h"
#include "../../translator/guest/x86_64/smc_address.h"
#include "../../translator/guest/x86_64/smc/protection.c"
#include "../../translator/guest/x86_64/dispatch.h" // x86 dispatch seam for the SHARED engine/dispatch.c
#else
#include "../../translator/guest/x86_64/interp_dispatch.h"
#endif
#define HL_GUEST_STAT_SIZE HL_LINUX_STAT_X86_64_SIZE
#define HL_GUEST_STAT_ENCODE hl_linux_stat_encode_x86_64
#define HL_GUEST_BOUND_STAT hl_linux_stat_x86_64
#include "../../linux_abi/guest_stat.h"
#undef HL_GUEST_BOUND_STAT
#undef HL_GUEST_STAT_ENCODE
#undef HL_GUEST_STAT_SIZE

// Byte size of the guest `struct stat` stat.c writes -- the shared stat syscalls (os/linux/syscall/
// fs.c cases 79/80) validate exactly this many guest bytes before filling the buffer (EFAULT guard).
#define GUEST_LINUX_STAT_BYTES 144

#include "../../linux_abi/container/state.c" // SHARED: container globals (rootfs/cwd/netns/ids/fd tables)
#include "../../linux_abi/fdcache.h"
#include "../../linux_abi/container/vfs/gmap.h"
#include "../../linux_abi/container/owner.h"
static uint64_t g_nonpie_lo, g_nonpie_hi, g_nonpie_bias;
#include "../../translator/guest/x86_64/avx.h"
static int jit86_avx_memory_read(uint64_t guest, void *destination, size_t length);
static int jit86_avx_memory_write(uint64_t guest, const void *source, size_t length);
static const hl_x86_avx_state g_avx_state = {&g_nonpie_lo, &g_nonpie_hi, &g_nonpie_bias, jit86_avx_memory_read,
                                             jit86_avx_memory_write};
#include "../../translator/guest/x86_64/glue.h" // independently compiled x86 target state
#include "../../translator/guest_fetch.h"
#include "../../translator/guest/x86_64/rep_runtime.h" // string-op helpers + the hooks engine_global_init sets
#include "../../translator/cache.c"                    // SHARED translator: code cache + block map

uint64_t hl_x86_guest_pointer(uint64_t address) {
    return g_nonpie_lo && address >= g_nonpie_lo && address < g_nonpie_hi ? address + g_nonpie_bias : address;
}

static int guestfold_on(void) {
    return g_nonpie_lo != 0;
}

static const hl_host_services *effective_host_services(void) {
    return hl_target_services_effective(&g_target_services);
}

static void jit86_store_alias_changed(uint64_t guest, size_t size);
static int jit86_store_alias_observation_active(void);
static void gbus_mapping_transition_lock(void);
static void gbus_mapping_transition_unlock(void);
static size_t x86_store_writable_prefix(uintptr_t address, size_t length);

static void jit86_smc_queue_range(uint64_t lo, uint64_t hi, void *opaque) {
    struct cpu *cpu = opaque;
    if (hi <= lo) return;
    if (lo == 0 && hi == UINT64_MAX) {
        cpu->smc_range_overflow = 1;
        return;
    }
    for (uint64_t index = 0; index < cpu->smc_range_count; ++index) {
        if (hi < cpu->smc_ranges[index][0] || lo > cpu->smc_ranges[index][1]) continue;
        if (lo < cpu->smc_ranges[index][0]) cpu->smc_ranges[index][0] = lo;
        if (hi > cpu->smc_ranges[index][1]) cpu->smc_ranges[index][1] = hi;
        return;
    }
    if (cpu->smc_range_count == X86_SMC_RANGE_CAP) {
        cpu->smc_range_overflow = 1;
        return;
    }
    cpu->smc_ranges[cpu->smc_range_count][0] = lo;
    cpu->smc_ranges[cpu->smc_range_count][1] = hi;
    cpu->smc_range_count++;
}

static int jit86_avx_memory_read(uint64_t guest, void *destination, size_t length) {
    hl_logical_vma_pin pin = {0};
    int logical = hl_logical_vma_pin_data(guest, length, HL_LOGICAL_VMA_READ, &pin);
    if (logical <= 0) return logical;
    if (pin.contiguous < length) {
        hl_logical_vma_unpin(&pin);
        return -1;
    }
    memcpy(destination, pin.host, length);
    hl_logical_vma_unpin(&pin);
    return 1;
}

static int jit86_avx_memory_write(uint64_t guest, const void *source, size_t length) {
    hl_logical_vma_pin pin = {0};
    int logical = hl_logical_vma_pin_data(guest, length, HL_LOGICAL_VMA_WRITE, &pin);
    if (logical < 0) return -1;
    if (logical == 0) {
        // AVX helpers execute in the dispatcher rather than translated code, so no emitted soft guard
        // protects this direct copy. Validate the complete guest span before touching its first byte;
        // otherwise a cross-page store partially commits and then faults in engine C code, where the
        // guest's synchronous-fault landing authority is not armed.
        if (!hl_x86_guest_writable(guest, length)) return -1;
        memcpy((void *)(uintptr_t)guest, source, length);
        jit86_store_alias_changed(guest, length);
        return 1;
    }
    if (pin.contiguous < length) {
        hl_logical_vma_unpin(&pin);
        return -1;
    }
    memcpy(pin.host, source, length);
    hl_logical_vma_unpin(&pin);
    jit86_store_alias_changed(guest, length);
    return 1;
}

/*
 * The translator's guest-memory seam (translator/guest_memory.h): the ledger
 * and the non-PIE window are engine knowledge, so the engine hands them over
 * rather than letting the translator archive link the Linux ABI.
 *
 * These are not the AVX pair above: an ORDINARY address is reported as 0 with
 * nothing copied, because the string-op caller must run its own direct-access
 * validator first, and an ordinary span that reaches into a logical VMA before
 * `length` is a fault rather than a raw host copy.
 */
static int jit86_guest_memory_read(uint64_t guest, void *destination, size_t length) {
    hl_logical_vma_pin pin = {0};
    int logical = hl_logical_vma_pin_data(guest, length, HL_LOGICAL_VMA_READ, &pin);
    if (logical < 0) return -1;
    if (pin.contiguous < length) {
        hl_logical_vma_unpin(&pin);
        return -1;
    }
    if (logical == 0) return 0;
    memcpy(destination, pin.host, length);
    hl_logical_vma_unpin(&pin);
    return 1;
}

static int jit86_guest_memory_write(uint64_t guest, const void *source, size_t length) {
    hl_logical_vma_pin pin = {0};
    int logical = hl_logical_vma_pin_data(guest, length, HL_LOGICAL_VMA_WRITE, &pin);
    if (logical < 0) return -1;
    if (pin.contiguous < length) {
        hl_logical_vma_unpin(&pin);
        return -1;
    }
    if (logical == 0) return 0;
    memcpy(pin.host, source, length);
    hl_logical_vma_unpin(&pin);
    return 1;
}

static int jit86_guest_memory_pin(uint64_t guest, size_t length, hl_guest_memory_access access,
                                  hl_guest_memory_pin *pin) {
    hl_logical_vma_pin logical_pin = {0};
    uint32_t required = access == HL_GUEST_MEMORY_WRITE ? HL_LOGICAL_VMA_WRITE : HL_LOGICAL_VMA_READ;
    int logical = hl_logical_vma_pin_data(guest, length, required, &logical_pin);
    if (logical < 0) return HL_GUEST_MEMORY_FAULT;
    size_t direct_contiguous = length;
    if (logical == 0) {
        if (access == HL_GUEST_MEMORY_WRITE) {
            direct_contiguous = x86_store_writable_prefix((uintptr_t)hl_x86_guest_pointer(guest), length);
            if (direct_contiguous == 0) return HL_GUEST_MEMORY_FAULT;
        } else if (!hl_x86_guest_readable(guest, length)) {
            return HL_GUEST_MEMORY_FAULT;
        }
    }
    pin->host = logical ? logical_pin.host : (void *)(uintptr_t)hl_x86_guest_pointer(guest);
    pin->contiguous = logical ? logical_pin.contiguous : direct_contiguous;
    pin->token = logical_pin.token;
    if (!logical && g_nonpie_lo) {
        uint64_t boundary = guest < g_nonpie_lo ? g_nonpie_lo : (guest < g_nonpie_hi ? g_nonpie_hi : UINT64_MAX);
        if (boundary > guest && boundary - guest < pin->contiguous) pin->contiguous = (size_t)(boundary - guest);
    }
    return logical;
}

static void jit86_guest_memory_unpin(hl_guest_memory_pin *pin) {
    if (pin->token == NULL) return;
    hl_logical_vma_pin logical_pin = {.token = pin->token};
    hl_logical_vma_unpin(&logical_pin);
}

static const hl_guest_memory_ops g_guest_memory_ops = {
    .resolve_exec = hl_logical_vma_resolve_exec,
    .read = jit86_guest_memory_read,
    .write = jit86_guest_memory_write,
    .indirect = hl_logical_vma_global_active,
    .host_pointer = hl_x86_guest_pointer,
    .exec_span = hl_logical_vma_resolve_exec_span,
    .exec_generation = hl_logical_vma_global_exec_generation,
    .pin = jit86_guest_memory_pin,
    .unpin = jit86_guest_memory_unpin,
    .transaction_begin = gbus_mapping_transition_lock,
    .transaction_end = gbus_mapping_transition_unlock,
    .store_observe = jit86_store_alias_changed,
};

// Host-CPU fork: an AArch64 host takes the x86-64 -> ARM64 translator below (register model at the top of
// this file); any other takes interp.c, which decodes x86-64 directly. Both share struct cpu: it is the
// checkpoint format.
#if defined(HL_HOST_CPU_AARCH64)
static int g_address_recorded;
#include "../../translator/guest/x86_64/emit.c" // x86 engine: arm64 emitters + SSE + x87
#include "../../translator/guest/x86_64/address.h"

static void address_addi(void *context, int rd, int rn, unsigned immediate, int sf, int shift) {
    (void)context;
    if (shift)
        e_addi_sh(rd, rn, immediate, sf, shift);
    else
        e_addi(rd, rn, immediate, sf);
}

static void address_subi(void *context, int rd, int rn, unsigned immediate, int sf, int shift) {
    (void)context;
    if (shift)
        e_subi_sh(rd, rn, immediate, sf, shift);
    else
        e_subi(rd, rn, immediate, sf);
}

static void address_movconst(void *context, int rd, uint64_t value) {
    (void)context;
    e_movconst(rd, value);
}

static void address_addreg(void *context, int rd, int rn, int rm, int sf, int shift) {
    (void)context;
    e_rrr(A_ADD, rd, rn, rm, sf, shift);
}

static void address_lsr(void *context, int rd, int rn, int shift, int sf) {
    (void)context;
    e_lsr_i(rd, rn, shift, sf);
}

static void address_movreg(void *context, int rd, int rn, int sf) {
    (void)context;
    e_mov_rr(rd, rn, sf);
}

static void address_movzero(void *context, int rd, uint32_t immediate, int shift) {
    (void)context;
    e_movz(rd, immediate, shift);
}

static void address_uxt(void *context, int rd, int rn, int bytes) {
    (void)context;
    e_uxt(rd, rn, bytes);
}

static void address_load_cpu(void *context, int rt, int offset) {
    (void)context;
    e_ldr(rt, 28, offset);
}

static void address_load_scaled(void *context, int width, int rt, int rn, unsigned offset) {
    (void)context;
    e_load_uoff(width, rt, rn, offset);
}

static void address_load_unscaled(void *context, int width, int rt, int rn, int offset) {
    (void)context;
    e_ldur(width, rt, rn, offset);
}

static void address_load(void *context, int width, int rt, int rn) {
    (void)context;
    e_load(width, rt, rn);
}

static void address_record_guest(void *context, int reg, int rip_relative) {
    (void)context;
    (void)rip_relative;
    // Effective addresses are architectural guest coordinates. Displaced ET_EXEC PCs are now
    // canonical throughout x86 lowering, so a RIP-relative address is already LOW; subtracting the
    // storage bias here wrapped it into 0xffff... and exposed that engine-private value as si_addr.
    e_str(reg, 28, OFF_SOFT_GUEST_EA);
    g_address_recorded = 1;
}

static void address_bus_guard(void *context, int reg, uint64_t size, uint64_t pc) {
    (void)context;
    emit_bus_guard(reg, size, pc);
}

static uintptr_t address_branch_placeholder(void *context) {
    (void)context;
    uint32_t *placeholder = (uint32_t *)g_cp;
    emit32(0);
    return (uintptr_t)placeholder;
}

static void address_patch_cbnz(void *context, uintptr_t token, int reg) {
    (void)context;
    uint32_t *placeholder = (uint32_t *)token;
    *placeholder = UINT32_C(0xB5000000) |
                   (((uint32_t)(((uint8_t *)g_cp - (uint8_t *)placeholder) / 4) & UINT32_C(0x7FFFF)) << 5) |
                   (uint32_t)reg;
}

static const hl_x86_address_emitter address_emitter = {
    address_addi,          address_subi,    address_movconst,     address_addreg,    address_lsr,
    address_movreg,        address_movzero, address_uxt,          address_load_cpu,  address_load_scaled,
    address_load_unscaled, address_load,    address_record_guest, address_bus_guard, address_branch_placeholder,
    address_patch_cbnz};

static hl_x86_address_state address_state(void) {
    return (hl_x86_address_state){NULL,   &address_emitter, g_nonpie_lo, g_nonpie_hi,          g_nonpie_bias,
                                  OFF_FS, OFF_GS,           1,            jit_guest_bus_active()};
}

void emit_ea_core(struct insn *insn, uint64_t next, int bias) {
    hl_x86_address_state state = address_state();
    hl_x86_address_emit(&state, insn, next, bias);
}

void emit_ea(struct insn *insn, uint64_t next) {
    emit_ea_core(insn, next, 1);
}

int ea_reg_fold(struct insn *insn, int width, int *rn, int *rm, int *shift) {
    hl_x86_address_state state = address_state();
    return hl_x86_address_fold_reg(&state, insn, width, rn, rm, shift);
}

int ea_imm_fold(struct insn *insn, int width, int *rn, int *offset) {
    hl_x86_address_state state = address_state();
    return hl_x86_address_fold(&state, insn, width, rn, offset);
}

void emit_load_mem(struct insn *insn, uint64_t next, int width, int rt) {
    hl_x86_address_state state = address_state();
    hl_x86_address_load(&state, insn, next, width, rt);
}

#include "../../translator/guest/x86_64/translate.c" // x86-64 translate_block + trampolines
#include "../../translator/guest/x86_64/cache.c"     // persistent translated-code cache (HL_PCACHE=1)

// The same-ISA transliterator is a THIRD arm of this fork and belongs to the interpreter's side of it
// (translator/guest/x86_64/translit.inc, included by interp.c). The two hooks the rest of this file calls
// exist here too, so the ARM64 host arm needs no #ifdef at either call site.
static int translit_enabled(void) {
    return 0;
}

#else
// interp.c defines the same names emit.c/translate.c/cache.c do, so everything below is host-identical.
static int x86_guest_fetch_exec(uint64_t guest, void *destination, size_t length);
#include "../../translator/guest/x86_64/interp.c"

// The interpreter does not protect translated source pages because it emits
// no translated code.  Preserve the shared store-publication call site while
// making its translation-invalidation predicate explicitly empty.
static int smc_tracked_written(uint64_t address, uint64_t size) {
    (void)address;
    (void)size;
    return 0;
}
#endif
#include "../../linux_abi/thread.c" // SHARED: clone->pthread, per-thread cpu, futex

/*
 * Queue the bytes a store actually wrote into an emulated MAP_SHARED mapping,
 * for jit86_smc_commit to write back before the next syscall can notify a
 * peer.  Only ever called with the range the guest really stored -- see the
 * store_ranges comment in cpu.h for why the alias ranges must stay out.
 */
static void jit86_store_writeback_record(struct cpu *cpu, uint64_t lo, uint64_t hi) {
    for (uint64_t index = 0; index < cpu->store_range_count; ++index) {
        if (hi < cpu->store_ranges[index][0] || lo > cpu->store_ranges[index][1]) continue;
        if (lo < cpu->store_ranges[index][0]) cpu->store_ranges[index][0] = lo;
        if (hi > cpu->store_ranges[index][1]) cpu->store_ranges[index][1] = hi;
        return;
    }
    if (cpu->store_range_count == X86_STORE_RANGE_CAP) {
        /*
         * Full.  Publish the oldest range now and reuse its slot: the store it
         * describes has already happened, so writing it back early is sound,
         * and it keeps the queue EXACT.  Coalescing distant ranges instead
         * would fabricate a span covering bytes the guest never wrote, which
         * is the very thing this queue exists to avoid.
         */
        filemap_flush_emulated(cpu->store_ranges[0][0], cpu->store_ranges[0][1]);
        memmove(cpu->store_ranges, cpu->store_ranges + 1, (X86_STORE_RANGE_CAP - 1) * sizeof cpu->store_ranges[0]);
        cpu->store_range_count--;
    }
    cpu->store_ranges[cpu->store_range_count][0] = lo;
    cpu->store_ranges[cpu->store_range_count][1] = hi;
    cpu->store_range_count++;
}

static void jit86_store_alias_ranges(struct cpu *cpu, uint64_t ranges[][2], uint32_t range_count, int emulated_store) {
    /* ranges[0] is the store itself; ranges[1..] are its aliases, which hold
       the bytes this store replaced and must never be written back. */
    if (emulated_store) jit86_store_writeback_record(cpu, ranges[0][0], ranges[0][1]);
    for (uint32_t r = 0; r < range_count; ++r) {
        uint64_t lo = ranges[r][0], hi = ranges[r][1];
        /* Shared-file identity alone does not make an address executable.
           Logical executable aliases were queued by
           hl_logical_vma_visit_exec_aliases above; among direct aliases only
           pages admitted to the SMC tracker can own translations.  Do not
           turn every ordinary MAP_SHARED store into an SMC exit and global
           mapping STW. */
        if (!smc_tracked_written(lo, hi - lo)) continue;
        int merged = 0;
        for (uint64_t i = 0; i < cpu->smc_range_count; ++i) {
            if (hi < cpu->smc_ranges[i][0] || lo > cpu->smc_ranges[i][1]) continue;
            if (lo < cpu->smc_ranges[i][0]) cpu->smc_ranges[i][0] = lo;
            if (hi > cpu->smc_ranges[i][1]) cpu->smc_ranges[i][1] = hi;
            merged = 1;
            break;
        }
        if (merged) continue;
        if (cpu->smc_range_count == X86_SMC_RANGE_CAP) {
            cpu->smc_range_overflow = 1;
            break;
        }
        cpu->smc_ranges[cpu->smc_range_count][0] = lo;
        cpu->smc_ranges[cpu->smc_range_count][1] = hi;
        cpu->smc_range_count++;
    }
}

static uint64_t jit86_store_alias_segment(struct cpu *cpu, uint64_t cursor, uint64_t last) {
    uint64_t ranges[GNA_MAX + 1][2];
    uint32_t range_count = 0;
    uint64_t segment_first = last;
    uint64_t segment_last = last;
    uint64_t device = 0, inode = 0, offset = 0;
    int emulated_store = 0;

    pthread_mutex_lock(&g_filemap_lock);
    const struct guest_file_mapping *source = NULL;
    for (int index = 0; index < g_nfilemap; ++index) {
        const struct guest_file_mapping *mapping = &g_filemap[index];
        if (mapping->hi <= cursor || mapping->lo >= last) continue;
        uint64_t first = mapping->lo > cursor ? mapping->lo : cursor;
        if (source == NULL || first < segment_first) {
            source = mapping;
            segment_first = first;
        }
    }
    if (source != NULL) {
        segment_last = source->hi < last ? source->hi : last;
        if (source->shared) {
            uint64_t length = segment_last - segment_first;
            offset = source->offset + (segment_first - source->lo);
            device = source->device;
            inode = source->inode;
            emulated_store = source->emulated != 0;
            ranges[0][0] = segment_first;
            ranges[0][1] = segment_last;
            range_count = 1;
            uint64_t file_last = offset + length;
            for (int index = 0; index < g_nfilemap && range_count < GNA_MAX + 1; ++index) {
                const struct guest_file_mapping *mapping = &g_filemap[index];
                if (!mapping->shared || mapping->device != device || mapping->inode != inode) continue;
                uint64_t mapping_length = mapping->hi - mapping->lo;
                if (mapping->offset > UINT64_MAX - mapping_length) continue;
                uint64_t mapping_last = mapping->offset + mapping_length;
                uint64_t first = offset > mapping->offset ? offset : mapping->offset;
                uint64_t alias_last = file_last < mapping_last ? file_last : mapping_last;
                if (alias_last <= first) continue;
                uint64_t alias_first = mapping->lo + (first - mapping->offset);
                ranges[range_count][0] = alias_first;
                ranges[range_count][1] = alias_first + (alias_last - first);
                range_count++;
            }
        }
    }
    pthread_mutex_unlock(&g_filemap_lock);
    if (range_count != 0) jit86_store_alias_ranges(cpu, ranges, range_count, emulated_store);
    return segment_last;
}

static void jit86_store_alias_changed(uint64_t guest, size_t size) {
    if (size == 0 || guest > UINT64_MAX - size) return;
    struct cpu *cpu = pthread_getspecific(g_cpu_key);
    if (cpu == NULL) return;
    if (smc_tracked_written(guest, size)) jit86_smc_queue_range(guest, guest + size, cpu);
    if (hl_logical_vma_visit_exec_aliases(guest, guest + size, jit86_smc_queue_range, cpu)) return;
    if (!filemap_shared_filter_maybe(guest, size)) return;
    uint64_t last = guest + size;
    for (uint64_t cursor = guest; cursor < last;) {
        uint64_t next = jit86_store_alias_segment(cpu, cursor, last);
        if (next <= cursor) break;
        cursor = next;
    }
}

static int jit86_store_alias_observation_active(void) {
    return g_rwx_guest != 0 || filemap_emulated_shared_active();
}

static void jit86_smc_commit(struct cpu *cpu) {
    int invalidates_code = cpu->smc_range_count != 0 || cpu->smc_range_overflow;
    if (invalidates_code) stw_mapping_begin();
    /* Writeback is driven by store_ranges, never by smc_ranges: an SMC range
       overflow means "drop every translation", but there is no matching
       conservative writeback -- flushing an unwritten range destroys data. */
    for (uint64_t index = 0; index < cpu->store_range_count; ++index)
        filemap_flush_emulated(cpu->store_ranges[index][0], cpu->store_ranges[index][1]);
    cpu->store_range_count = 0;

    /* Publishing ordinary MAP_SHARED bytes does not change mapping or code
       authority.  filemap_flush_emulated serializes the mapping registry and
       backing write itself; stopping every translated thread here turns each
       observed store exit into a process-wide barrier.  Mapping STW above is
       therefore conditional on the SMC half actually mutating translation
       ingress. */
    if (!invalidates_code) return;
    uint32_t removed;
    if (cpu->smc_range_overflow) {
        removed = g_live_map_count;
        map_clear();
        memset(g_ibtc, 0, sizeof g_ibtc);
        memset(g_xibtc, 0, sizeof g_xibtc);
    } else {
        removed = map_invalidate_source_ranges((const uint64_t (*)[2])cpu->smc_ranges, (uint32_t)cpu->smc_range_count);
        if (removed) {
            memset(g_ibtc, 0, sizeof g_ibtc);
            memset(g_xibtc, 0, sizeof g_xibtc);
        }
    }
    (void)removed;
    cpu->smc_range_count = 0;
    cpu->smc_range_overflow = 0;
    stw_mapping_end();
}

#define HL_GUEST_SIGACTION_HAS_RESTORER 1
#define HL_DISPATCH_FAULT_ADDRESS(c)                                                                                   \
    ((c)->bus_ea != 0 && (c)->fault_addr == (c)->bus_ea ? (c)->soft_guest_ea : nonpie_unfold((c)->fault_addr))
#include "../../linux_abi/signal.c" // SHARED: signal delivery driver + translation

static size_t x86_store_writable_prefix(uintptr_t address, size_t length) {
#if defined(HL_NATIVE_TEST_HOOKS)
    if (g_test_store_preflight_active) {
        ++g_test_store_preflight_calls;
        return g_test_store_preflight_prefix < length ? g_test_store_preflight_prefix : length;
    }
#endif
    return host_range_writable_prefix(address, length);
}

static int x86_store_fault_is_protected(uint64_t address) {
#if defined(HL_NATIVE_TEST_HOOKS)
    if (g_test_store_preflight_active) return g_test_store_preflight_protected;
#endif
    return gna_hit(address, 1) || gro_hit(address, 1);
}

static int soft_tlb_miss(struct cpu *c) {
    uint64_t address = c->bus_ea;
    uint64_t width = c->soft_width;
    uint32_t required = (uint32_t)c->soft_required;
    const _Atomic(hl_logical_vma_snapshot *) *source = hl_logical_vma_global_snapshot_source();
    hl_logical_vma_snapshot *snapshot = atomic_load_explicit(source, memory_order_acquire);
    const hl_logical_vma_view *view = NULL;
    if (snapshot != NULL) {
        size_t low = 0, high = snapshot->count;
        while (low < high) {
            size_t middle = low + (high - low) / 2;
            const hl_logical_vma_view *candidate = &snapshot->views[middle];
            if (address < candidate->guest_first)
                high = middle;
            else if (address >= candidate->guest_last)
                low = middle + 1;
            else {
                view = candidate;
                break;
            }
        }
    }
    /* File EOF owns the overlapping inaccessible tail before the generic
       logical/host mapping classifier.  The host range is deliberately
       anonymous/protected to provide partial-page zero fill, so asking only
       host_range_mapped below misreports the Linux BUS_ADRERR contract as an
       unmapped-data SIGSEGV. */
    uint64_t host_address = address;
    if (view != NULL) {
        if (view->host_delta > UINT64_MAX - address) {
            c->fault_addr = address;
            return raise_guest_data_map_fault(c);
        }
        host_address += view->host_delta;
    }
    uint64_t bus_fault = jit_guest_bus_fault(host_address, width);
    if (bus_fault != 0) {
        c->fault_addr = bus_fault - (host_address - address);
        c->bus_ea = 0;
        return raise_guest_bus(c);
    }
    if (view != NULL) {
        if ((view->protection & required) != required) {
            c->fault_addr = address;
            return raise_guest_fetch_fault(c);
        }
        uint64_t last = view->guest_last;
        size_t index = (size_t)(view - snapshot->views) + 1;
        while (index < snapshot->count && snapshot->views[index].guest_first == last &&
               snapshot->views[index].host_delta == view->host_delta &&
               (snapshot->views[index].protection & required) == required) {
            last = snapshot->views[index++].guest_last;
        }
        if (width > last - address) {
#if defined(__APPLE__)
            uint64_t end = address + width;
            if (end < address) {
                c->fault_addr = address;
                return raise_guest_data_map_fault(c);
            }
            if (index < snapshot->count && snapshot->views[index].guest_first < end &&
                (snapshot->views[index].protection & required) != required) {
                c->fault_addr = snapshot->views[index].guest_first;
                return raise_guest_fetch_fault(c);
            }
            if (view->host_delta != 0 || (index < snapshot->count && snapshot->views[index].guest_first < end)) {
                c->reason = R_SOFTSPAN;
                return 0;
            }
            last = end;
#else
            c->reason = R_SOFTSPAN;
            return 0;
#endif
        }
        c->soft_delta = view->host_delta;
        c->soft_protection = view->protection;
        c->soft_last = last;
    } else {
        /* Ordinary directly represented guest page.  Native protection/fault
           handling remains authoritative after the identity rewrite. */
        c->soft_delta = 0;
        c->soft_protection = HL_LOGICAL_VMA_READ | HL_LOGICAL_VMA_WRITE | HL_LOGICAL_VMA_EXEC;
        if (width > UINT64_MAX - address) {
            c->fault_addr = address;
            return raise_guest_data_map_fault(c);
        }
        if (required & HL_LOGICAL_VMA_WRITE) {
            size_t writable = x86_store_writable_prefix((uintptr_t)nonpie_fold(address), (size_t)width);
            if (writable < width) {
                c->fault_addr = address + writable;
                if (x86_store_fault_is_protected(c->fault_addr)) return raise_guest_fetch_fault(c);
                return raise_guest_data_map_fault(c);
            }
#if !defined(__APPLE__)
        } else if (!host_range_mapped((uintptr_t)nonpie_fold(address), (size_t)width)) {
            uint64_t readable = gna_prefix(address, width);
            c->fault_addr = address + (readable < width ? readable : 0);
            return raise_guest_data_map_fault(c);
#endif
        }
        /* The string-op helper rejects a store into a read-only mapping itself (it copies with the host
           memcpy, several C frames below translated code, where a hardware fault is unattributable) and
           lands here.  Answering "writable" would re-run the same rejected element forever, so this is
           where a bulk store learns the same answer the MMU gives a scalar one: mapped, not writable. */
        if ((required & HL_LOGICAL_VMA_WRITE) && gro_hit(address, width)) {
            c->fault_addr = address;
            return raise_guest_fetch_fault(c);
        }
        uint64_t end = address + width;
        uint64_t last = (address & ~UINT64_C(4095)) + UINT64_C(4096);
        if (end > last) last = end;
        if (snapshot != NULL) {
            for (size_t index = 0; index < snapshot->count; ++index) {
                if (snapshot->views[index].guest_first < end && snapshot->views[index].guest_last > address) {
#if defined(__APPLE__)
                    const hl_logical_vma_view *overlap = &snapshot->views[index];
                    if ((overlap->protection & required) != required) {
                        c->fault_addr = overlap->guest_first > address ? overlap->guest_first : address;
                        return raise_guest_fetch_fault(c);
                    }
                    if (overlap->host_delta != 0) {
                        c->reason = R_SOFTSPAN;
                        return 0;
                    }
                    continue;
#else
                    c->reason = R_SOFTSPAN;
                    return 0;
#endif
                }
                if (snapshot->views[index].guest_first >= end && snapshot->views[index].guest_first < last)
                    last = snapshot->views[index].guest_first;
            }
        }
        c->soft_last = last;
    }
    c->soft_page = address & ~UINT64_C(4095);
    c->soft_snapshot = snapshot != NULL ? (uint64_t)(uintptr_t)snapshot : 1;
    c->reason = R_BRANCH;
    return 0;
}

#if defined(HL_NATIVE_TEST_HOOKS)
HL_API int hl_x86_64_store_preflight_test(void) {
    uint32_t code[256] = {0};
    uint8_t *saved_cp = g_cp;
    int saved_recorded = g_address_recorded;
    int saved_rwx = g_rwx_guest;
    uint64_t saved_handler = g_sigact[11].handler;
    g_cp = (uint8_t *)code;
    g_address_recorded = 0;
    g_rwx_guest = 0;
    g_test_direct_store_guard_emissions = 0;
    emit_memory_guard(17, 8, UINT64_C(0x1000), X86_SOFT_WRITE);
    emit_memory_guard(17, 16, UINT64_C(0x2000), X86_SOFT_WRITE);
    int emitted = g_test_direct_store_guard_emissions == 2;
    g_cp = saved_cp;
    g_address_recorded = saved_recorded;
    g_rwx_guest = saved_rwx;

    unsigned char bytes[32];
    memset(bytes, 0xa5, sizeof bytes);
    g_sigact[11].handler = 2;
    g_test_store_preflight_active = 1;
    g_test_store_preflight_calls = 0;
    int valid = emitted;
    for (size_t width = 8; width <= 16; width *= 2) {
        for (int classification = 0; classification < 3; ++classification) {
            struct cpu cpu;
            memset(&cpu, 0, sizeof cpu);
            uint64_t address = (uint64_t)(uintptr_t)bytes;
            g_test_store_preflight_prefix = width / 2;
            /* Exercise absent, protected and read-only second-page classifications. */
            g_test_store_preflight_protected = classification != 0;
            cpu.bus_ea = address;
            cpu.soft_guest_ea = address;
            cpu.soft_width = width;
            cpu.soft_required = X86_SOFT_WRITE;
            int result = soft_tlb_miss(&cpu);
            valid &= result == 1 && cpu.fault_addr == address + width / 2 && cpu.sync_address == address + width / 2 &&
                     cpu.sync_code == (classification == 0 ? 1 : 2);
            for (size_t index = 0; index < sizeof bytes; ++index)
                valid &= bytes[index] == 0xa5;
        }
    }
    valid &= g_test_store_preflight_calls == 6;
    g_test_store_preflight_active = 0;
    g_sigact[11].handler = saved_handler;
    return valid ? 0 : 1;
}
#endif

static int x86_signal_cache_contains(void *context, uint64_t pc) {
    (void)context;
    return jit_pc_in_retained_cache(pc);
}

static uint64_t x86_signal_handler(void *context, int signal_number) {
    (void)context;
    return g_sigact[signal_number].handler;
}

static hl_x86_signal_queue x86_signal_queue(void) {
    return (hl_x86_signal_queue){x86_signal_handler, NULL, g_sigcode, g_sigaddr, &g_pending};
}

static void build_signal_frame(struct cpu *c, int sig, int synchronous) {
    hl_x86_signal_state state = {
        .handler = g_sigact[sig].handler,
        .flags = g_sigact[sig].flags,
        .mask = g_sigact[sig].mask,
        .error = &g_sigerror[sig],
        .code = synchronous ? &c->sync_code : &g_sigcode[sig],
        .value = &g_sigval[sig],
        .address = synchronous ? &c->sync_address : &g_sigaddr[sig],
        .pid = &g_sigpid[sig],
        .uid = &g_siguid[sig],
        // x86 glibc supplies SA_RESTORER.  Returning through its real guest
        // trampoline gives forced unwinders valid CFI instead of the engine's
        // unreadable sentinel; fall back for raw actions without a restorer.
        .sigreturn_pc = sig == 32 && (g_sigact[sig].flags & UINT64_C(0x04000000)) && g_sigact[sig].restorer
                            ? g_sigact[sig].restorer
                            : SIGRETURN_PC,
        .trace = 0,
    };
    hl_x86_signal_build(c, sig, &state);
}

static void do_sigreturn(struct cpu *c) {
    hl_x86_signal_restore(c);
}

// Fault capture is per BACKEND: the JIT reconstructs guest state from the host mcontext and refines a
// block-granular host PC via the provenance map, which the interpreter must NOT use (it maps HOST addresses
// and this backend emits none; cpu->rip is already current). Both return 0 for "not a guest fault".
#if defined(HL_HOST_CPU_AARCH64)
static int sigframe_capture_fault(struct cpu *c, void *native_context) {
    if (!hl_x86_signal_capture(c, native_context, x86_signal_cache_contains, NULL)) return 0;
    // Recover the EXACT faulting guest RIP from the per-instruction provenance map (translate.c records
    // the host code range of each memory-accessing insn). Without this the mcontext RIP is only
    // block-granular; a crash reporter (breakpad/sentry) or a JIT that maps the trapping PC back to a
    // bytecode site would misreport. Falls back to the block-granular cpu->rip when unmapped.
    uint64_t exact_pc;
    uint64_t host_pc = (uint64_t)HL_HOST_UC_PC((ucontext_t *)native_context);
    if (jit_instruction_guest_pc(host_pc, &exact_pc)) c->rip = exact_pc;
    return 1;
}
#else
static int sigframe_capture_fault(struct cpu *c, void *native_context) {
    return interp_signal_capture(c, native_context);
}
#endif

// The JIT returns into block_return to unwind the translated frame; the interpreter siglongjmps to run_block.
#if defined(HL_HOST_CPU_AARCH64)
static void sigframe_resume_dispatch(struct cpu *c, void *native_context) {
    hl_x86_signal_resume(c, native_context, (uintptr_t)block_return);
}
#else
static void sigframe_resume_dispatch(struct cpu *c, void *native_context) {
    interp_signal_resume(c, native_context);
}
#endif

static int fastclk_fault_fixup(siginfo_t *info, void *native_context) {
    struct cpu *c = (struct cpu *)pthread_getspecific(g_cpu_key);
    return hl_x86_signal_fast_clock_fault(c, (uintptr_t)(info != NULL ? info->si_addr : NULL), native_context);
}

// x86 integer #DE (divide-by-zero / INT_MIN-over-minus-one overflow). si_code is FPE_INTDIV(1) for a zero
// divisor or FPE_INTOVF(2) for a quotient-overflow -- Linux/x86 reports FPE_INTDIV for the #DE trap in both
// cases, but the queued value is honoured for a guest handler. Returns 1 when a handler was queued (caller
// resumes). With no handler this owns the default disposition exactly like raise_guest_bus does for a
// past-EOF SIGBUS: on Linux the guest process is a real host process, so raise the host SIGFPE and record
// the intended Linux termination so the parent's wait4/waitid reconstructs WIFSIGNALED/WTERMSIG==SIGFPE
// (the dispatcher used to set a plain exit_code=136, i.e. WIFEXITED, which is not a signal-death).
static int raise_guest_de(struct cpu *c, int si_code) {
    hl_x86_signal_queue queue = x86_signal_queue();
    if (hl_x86_signal_raise_divide(c, &queue, si_code)) return 1;
    if (container_pid() != 1) {
#if defined(__linux__)
        signal(SIGFPE, SIG_DFL);
        raise(SIGFPE);
#endif
        int core = sig_coredumps(8) && svc_core_rlimit_cur() > 0;
        sigexit_record(8, core);
    }
    c->exited = 1;
    c->exit_code = 136; // 128 + SIGFPE, the container-init / relay-exhausted fallback
    return 0;
}

static int raise_guest_trap(struct cpu *c) {
    hl_x86_signal_queue queue = x86_signal_queue();
    if (hl_x86_signal_raise_trap(c, &queue)) return 1;
    // A translated #BP/#UD with the default disposition is a guest signal
    // death, not a normal exit whose numeric code happens to be 128+signal.
    // Publish the signal record before terminating so --report-exit and
    // parent wait semantics retain WIFSIGNALED/WTERMSIG.
    guest_group_fatal(c, (int)(c->divop & 0xff));
}

static uint64_t nzcv_to_eflags(uint64_t nzcv) {
    return hl_x86_signal_nzcv_to_eflags(nzcv);
}

static uint64_t eflags_to_nzcv(uint64_t eflags) {
    return hl_x86_signal_eflags_to_nzcv(eflags);
}

#include "../../linux_abi/container/vfs.c"   // SHARED: rootfs jail, overlay, /proc synth, stat
#include "../../linux_abi/container/netns.c" // SHARED: sockets, loopback netns, termios
#include "../../linux_abi/image.h"
static void load_elf(const char *path, struct loaded *out, const void *placement, const hl_linux_image *pinned);
static int elf_interp(const char *path, char *out, size_t n, const hl_linux_image *pinned);
static uint64_t build_stack(int argc, char **argv, struct loaded *lm, uint64_t at_base);

static int64_t legacy_time_seconds(void *context) {
    (void)context;
    return (int64_t)time(NULL);
}

// legacy.c dereferences a few guest pointers itself (arch_prctl GET_FS/GS, time's tloc, the legacy
// timeval/utimbuf buffers). Same span validity the syscall handlers use, injected because the translator
// may not reach into linux_abi. `address` arrives already folded to storage.
static int legacy_access_ok(void *context, uint64_t address, uint64_t length, int write) {
    (void)context;
    return write ? host_range_writable((uintptr_t)address, (size_t)length)
                 : host_range_mapped((uintptr_t)address, (size_t)length);
}

static int legacy_set_alarm(void *context, uint64_t seconds, uint64_t *remaining_seconds) {
    struct itimerval next = {{0, 0}, {0, 0}};
    struct itimerval previous = {{0, 0}, {0, 0}};
    (void)context;
    next.it_value.tv_sec = (time_t)seconds;
    if (setitimer(ITIMER_REAL, &next, &previous) < 0) return errno;
    *remaining_seconds = (uint64_t)previous.it_value.tv_sec + (previous.it_value.tv_usec != 0 ? 1u : 0u);
    return 0;
}

static char g_authorized_executable_path[4200];
static const void *g_authorized_executable_image;
static size_t g_authorized_executable_size;
static void *g_authorized_executable_owned;
static struct stat g_authorized_executable_status;
static hl_dac_snapshot g_authorized_executable_dac;
static hl_exec_file_capabilities g_authorized_executable_file_capabilities;
static int g_authorized_executable_metadata_ready;
#include "../../linux_abi/syscall/dispatch.c" // SHARED: the canonical syscall layer
#include "../../linux_abi/sentry.c"           // untrusted-guest isolation: SPSC ring + sentry split (g_untrusted)
static void ckpt_poll(struct cpu *c);
#define G_CKPT_POLL(c) ckpt_poll(c)
#define G_CKPT_ARCH 1
#define G_CKPT_CPU_SANITIZE(c)                                                                                         \
    do {                                                                                                               \
        (c)->dbg_ibsrc = 0;                                                                                            \
        (c)->ic_miss = 0;                                                                                              \
        (c)->x87_ea = 0;                                                                                               \
        (c)->divop = 0;                                                                                                \
        (c)->ibtc_base = 0;                                                                                            \
        (c)->vdirty = 0;                                                                                               \
        (c)->fastclk_ptr = 0;                                                                                          \
        (c)->fastclk_resume = 0;                                                                                       \
        (c)->fault_addr = 0;                                                                                           \
        (c)->bus_ea = 0;                                                                                               \
        (c)->soft_guest_ea = 0;                                                                                        \
        (c)->bus_filter = 0;                                                                                           \
        (c)->bus_force = 0;                                                                                            \
        memset((c)->bus_scratch, 0, sizeof(c)->bus_scratch);                                                           \
        G_SOFT_TLB_CLEAR(c);                                                                                           \
    } while (0)
static int container_init(const char *rootfs);
static int engine_global_init(void);
#include "../dispatch.c" // SHARED engine: run_guest loop (x86 drives it via dispatch.h;
// keeps its own run_block/block_return in translate.c, G_OWN_TRAMPOLINES)
static const void *g_initial_executable_image;
static size_t g_initial_executable_size;
static const void *g_initial_interpreter_image;
static size_t g_initial_interpreter_size;
static uint64_t g_loaded_image_identity;
#include "../../linux_abi/x86.c" // Linux x86-64 ELF loader + stack + fault handlers
#include "../../linux_abi/checkpoint.c"

// ---------------- entry ----------------
static int g_engine_inited;

static int container_init(const char *rootfs) {
    g_rootfs_mode = rootfs != NULL && rootfs[0] != 0;
#if defined(__APPLE__)
    hl_linux_dns_prepare();
#endif
    hl_gmap_bind_limits(&g_limits);
    // PID ns: only containers (rootfs) get PID 1. Record the init's real host pid so the shared Linux
    // personality can virtualize just the init's identity (getpid()==1, host pgid<->guest pgid 1) and
    // pass real child pids straight through -- this is what makes bash job control (setpgid / TIOCSPGRP)
    // work on x86-64 the way it already does on aarch64. Without it g_init_hostpid stayed 0, getpid()
    // returned the real host pid, and bash's setpgid(0,1)/tcsetpgrp targeted host pid 1 (launchd) -> the
    // foreground command got SIGTTOU/SIGTTIN-stopped ("[N]+ Stopped  ls") instead of running.
    if (rootfs) g_init_hostpid = getpid();
    // Every guest process gets a namespace-local pid, not only the init (state.c).
    if (rootfs && container_pid_namespace_begin() != 0) return -1;
    // Cross-process cgroup accounting: a fresh shared slot table for this container init is inherited
    // by every guest fork (see state.c).
    if (rootfs) acct_container_reset(effective_host_services());
    container_read_resource_env(); // Docker CPU, read-only-root, and ulimit values from centralized HL options.
    // The final typed launch hands the container model to the engine as HL options, not as the
    // --hostname/--mem-max/--pids-max CLI flags. aarch64's container_init() already reads these options;
    // (linux_aarch64.c); x86-64 did not, so a `docker run --hostname h` on x86 dropped the hostname
    // (uname/gethostname/`/etc/hostname` returned "jit") and --memory/--pids-limit were ignored. The
    // Guard on an existing value so explicitly supplied configuration still wins.
    if (hl_option_get("HL_NET_HOST") == NULL) {
        const char *h = hl_option_get("HL_HOSTNAME");
        if (h && h[0] && !g_hostname[0]) {
            strncpy(g_hostname, h, 64);
            g_hostname[64] = 0;
        }
        const char *m = hl_option_get("HL_MEM_MAX");
        if (m && m[0] && !g_mem_max) g_mem_max = parse_size(m);
        const char *p = hl_option_get("HL_PIDS_MAX");
        if (p && p[0] && !g_pids_max) g_pids_max = hl_parse_id("HL_PIDS_MAX", p);
    }
    if (rootfs && rootfs[0]) { // the shared container jails against the canonical rootfs + its dir fd
        g_rootfs = (char *)rootfs;
        if (root_handle_bind(g_rootfs) != 0 || root_native_require() != 0) return -1;
        container_populate_dev();        // /dev/{fd,stdin,stdout,stderr,ptmx,pts,shm,console,...} the unpacker stripped
        container_populate_machine_id(); // /etc/machine-id agreeing with boot_id (if image ships none)
        // Container identity = root (0) by default; HL_UID/HL_GID or typed launch fields override it.
        const char *eu = hl_option_get("HL_UID");
        if (eu) g_uid = hl_parse_id("HL_UID", eu);
        const char *eg = hl_option_get("HL_GID");
        if (eg) g_gid = hl_parse_id("HL_GID", eg);
        if (g_uid < 0) g_uid = 0;
        if (g_gid < 0) g_gid = 0;
        cred_reset_initial();
    }
    if (!rootfs && (root_handle_bind("/") != 0 || hl_owner_seed("/", NULL, NULL, 0) != 0)) return -1;
    {
        // HL_NETNS is a short key (not a path) used to derive abstract-socket and IPC identities.
        // The daemon and both guest ISAs share it across exec; the private-loopback directory is derived from it.
        // Inherit the key when supplied, otherwise mint one from the process id.
        const char *ns = hl_option_get("HL_NETNS");
        char key[40];
        if (ns && ns[0])
            snprintf(key, sizeof key, "%.39s", ns);
        else
            snprintf(key, sizeof key, "%d", (int)getpid());
        namespace_key_set(key);
        snprintf(g_netns, sizeof g_netns, "/tmp/.hl-net-%.40s", key);
        if (hl_target_services_make_directory(&g_target_services, g_netns, 0700) == 0 && !(ns && ns[0]))
            hl_option_set("HL_NETNS", key, 1);
    }
    {
        const char *vs =
            hl_option_get("HL_VOLUMES"); // bind-mount volumes (env path; bridge usually can't pass env, so --vol too)
        if (vs && vs[0]) {
            char *tmp = strdup(vs);
            if (tmp == NULL) return -1;
            char *sv;
            for (char *t = strtok_r(tmp, ",", &sv); t; t = strtok_r(NULL, ",", &sv))
                add_vol(t);
            free(tmp);
        }
    }
    if (name_binds_parse(hl_option_get("HL_NAME_BINDS")) != 0) return -1;
    {
        const char *pub = hl_option_get("HL_PUBLISH");
        if (pub && pub[0] && hl_linux_ports_count(&g_ports) == 0) parse_publish(pub);
    } // docker -p (inherit across exec)
    {
        const char *ls = hl_option_get("HL_LOWER"); // overlay lower layers (inherit across exec)
        if (ls && ls[0] && !g_nlower) {
            char tmp[4096];
            snprintf(tmp, sizeof tmp, "%s", ls);
            char *sv;
            // HL_LOWER is a newline-record option, so host paths may contain ':'.
            for (char *t = strtok_r(tmp, "\n", &sv); t; t = strtok_r(NULL, "\n", &sv))
                add_lower(t);
        }
    }
    if (g_rootfs) {
        const char *owner_lowers[8];
        for (int i = 0; i < g_nlower; ++i)
            owner_lowers[i] = g_lower[i].canon;
        if (hl_owner_seed(g_rootfs, hl_option_get("HL_FILE_OWNERS"), owner_lowers, (size_t)g_nlower) != 0) return -1;
    }
    if (g_rootfs && chdir(g_rootfs) != 0) return -1; // guest cwd "/" maps to the rootfs root
    // Docker -w / initial working directory: start the guest in HL_CWD (must be reachable inside the
    // container -- typically a bind-mounted volume). confine() normalizes + clamps it to the rootfs.
    const char *icwd = hl_option_get("HL_CWD");
    if (icwd && icwd[0]) confine(icwd, g_cwd, sizeof g_cwd);
    // derive the run user's supplementary group set from the image rootfs (runc additionalGids), after
    // g_uid/g_gid + the overlay lowers are resolved, so getgroups(2) and /proc/self/status Groups: report it.
    if (g_rootfs) container_parse_groups();
    // The container identity this process runs under is now decided, so publish the one number a
    // checkpoint preserves for it. A launched member's guest pid is what the image names its group by
    // (`proc.<guest pid>`) and what a restore re-forks it under, so it is the only identity a host can
    // hold across a capture. Both target arms publish it; keep them in step.
    hl_engine_child_result_publish_guest_pid(ckpt_image_self_gpid());
    return 0;
}

// W3D: idempotent engine init (pthread key + MAP_JIT arena + trace env + fault handlers). Returns 0
// on success, nonzero exit code on failure. First call wins; later calls are no-ops (g_engine_inited),
// so the resident parent pays this once and the standalone path runs it exactly as before.
static int guest_fetch_direct_valid(uint64_t address, size_t length) {
    return host_range_mapped((uintptr_t)nonpie_fold(address), length);
}

static int guest_store_direct_valid(uint64_t address, size_t length) {
    return x86_store_writable_prefix((uintptr_t)nonpie_fold(address), length) == length;
}

static int x86_guest_fetch_exec(uint64_t guest, void *destination, size_t length) {
    return hl_guest_fetch_exec(nonpie_fold(guest), destination, length);
}

static int guest_rep_access_special(uint64_t address, size_t length, int write) {
    return gna_hit(address, (uint64_t)length) || hl_linux_bus_hit(address, (uint64_t)length) ||
           (write && gro_hit(address, (uint64_t)length));
}

static int engine_global_init(void) {
    hl_x86_decode_set_instruction_fetch(x86_guest_fetch_exec);
    hl_guest_memory_bind(&g_guest_memory_ops);
    hl_guest_fetch_set_direct_validator(guest_fetch_direct_valid);
    hl_x86_rep_set_store_commit(jit86_store_alias_changed, jit86_store_alias_observation_active);
    hl_x86_rep_set_access_validators(guest_fetch_direct_valid, guest_store_direct_valid, guest_rep_access_special);
    if (hl_target_services_bind(&g_target_services) != 0) return 1;
    if (g_engine_inited) return 0;
    if (pthread_key_create(&g_cpu_key, NULL) != 0) {
        perror("pthread_key_create");
        return 1;
    }
    // macOS host services own code mappings and post-fork alias repair. NODUALMAP keeps the single-MAP_JIT
    // compatibility mode; the default remains a stable dual RW/RX mapping.
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
    g_prof = hl_option_get("HL_C_DIAGNOSTICS") != NULL;
    g_profile_output_owner = 1;
    g_dispatch_diagnostics = g_prof;
    g_fwdskip = 8;
    extern void jit86_lazyguard(int, siginfo_t *, void *);
#if defined(_WIN32)
    // One process-wide vectored exception handler in place of two sigactions. It is not a preference: a
    // deliberate probe read is issued from between translated blocks and an absolute-data fixup can fire
    // from anywhere, so no frame-scoped mechanism spans the fault sites this engine actually has, while a
    // vectored handler fires on every thread wherever the fault happened -- which is the property a POSIX
    // signal handler has and the reason the classifier below is the SAME function the POSIX arms install.
    //
    // The classifier is fed a synthesized siginfo_t rather than being rewritten to speak the host fault
    // record, because the host record's kind/code fields are already numerically the Linux signal number
    // and si_code for the pair -- so the translation is a struct fill, and the alternative would be a
    // second copy of a 200-line classifier that must stay in step with the first.
    if (!hl_windows_fault_install(hl_windows_guest_fault, NULL)) {
        fprintf(stderr, "hl-engine: unable to install the fault handler\n");
        return 1;
    }
#else
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    // SA_ONSTACK only for the transliterator: it is the one backend whose HOST stack IS the guest stack, so
    // a guest stack overflow leaves no room to build the SIGSEGV frame and the handler that would deliver
    // the guest's signal never runs. The interpreter runs on its own host stack and does not need it, and
    // adding the flag there would change an existing lane for nothing.
    sa.sa_flags = SA_SIGINFO | (translit_enabled() ? SA_ONSTACK : 0);
    sa.sa_sigaction = jit86_lazyguard;
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL);
#endif
    // Untrusted-guest isolation (the sentry process-split). OFF by default -> trusted path unchanged.
    g_untrusted = hl_option_get("HL_UNTRUSTED") != NULL;
    g_sentry_sandbox = hl_option_get("HL_SANDBOX") != NULL;
    // ptrace tracer/tracee coordination arena -- mmap the shared region ONCE here, BEFORE any guest
    // fork, so every descendant guest process inherits the same physical pages. Inert until a guest ptraces.
    ptrace_arena_init();
    // POSIX record locks and BSD flock ownership share a process-tree broker. Embedded builds skip the
    // standalone constructor, so initialize it explicitly before any guest fork can inherit this engine.
    if (poslk_init() != 0) return 70;
    if (ckpt_control_init() != 0) return 70;
    g_engine_inited = 1;
    return 0;
}

// Load the main program and optional interpreter, recording their entry metadata. The gb/pb/ib
// buffers are static because g_exe_path points into gb and must outlive this call.
static const char *load_program(const char *prog, struct loaded *lm, struct loaded *li, uint64_t *jump,
                                uint64_t *at_base, int *have_interp, const hl_engine_main_image_plan *image_plan) {
    static char gb[1024];
    prog = find_in_path(prog, gb, sizeof gb);   // bare "sh" (docker) -> "/bin/sh" via the container PATH
    if (!g_comm_store[0]) set_guest_comm(prog); // record the pre-shebang name; preload lands here
    g_exe_path = prog;
    // /proc/self/exe must be the ABSOLUTE, CANONICAL guest path of the loaded image: a RELATIVE guest
    // invocation ("./x" from a harness) or an entry symlink otherwise leaks into the link value, and
    // glibc static-pie ASSERTS on it at startup ("dl-origin.c: linkval[0]=='/'"). Static: the
    // value must outlive this call, like gb above.
    static char bootexe[4200];
    exe_canon(prog, bootexe, sizeof bootexe);
    g_exe_path = bootexe;

    static char pb[4200];
    const char *prog_host =
        g_initial_executable_image != NULL ? prog : xresolve_overlay(prog, pb, sizeof pb); // named fallback only
    // Authorize the launched image for the bare-mode execve gate (proc.c) and the by-path image reader.
    // This must be set even when no executable image was embedded (the macOS embedding/bridge path launches
    // the guest purely by path): otherwise a guest re-exec of /proc/self/exe fails the authorized-target
    // check with ENOENT. In the normal production path g_initial_executable_image is always set, so only the
    // embedded by-path case changes here.
    if (!g_authorized_executable_path[0]) {
        if (g_rootfs != NULL)
            snprintf(g_authorized_executable_path, sizeof g_authorized_executable_path, "%s", g_exe_path);
        else if (realpath(prog_host, g_authorized_executable_path) == NULL)
            snprintf(g_authorized_executable_path, sizeof g_authorized_executable_path, "%s", prog_host);
    }
    // opt8: load the guest image + interp at FIXED VAs so the translated arena is byte-identical across
    // runs (one-shot g_force_base, cleared inside load_elf). Checkpoints need the same deterministic layout.
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
    g_loadbase = lm->base;
    *jump = lm->entry;
    *at_base = 0;
    *have_interp = 0;
    const char *interp_host = NULL;
    uint64_t interpreter_identity = 0xABCDEFull;
    char interp[256];
    int has_interp = elf_interp(prog_host, interp, sizeof interp, NULL) == 0;
    g_initial_executable_image = NULL;
    g_initial_executable_size = 0;
    if (has_interp) {
        static char ib[4200];
        interp_host = g_initial_interpreter_image != NULL ? interp : xresolve_overlay(interp, ib, sizeof ib);
        g_initial_executable_image = g_initial_interpreter_image;
        g_initial_executable_size = g_initial_interpreter_size;
        if (g_pcache || hl_option_get("HL_CHECKPOINT")) g_force_base = PC_INTERP_BASE;
        load_elf(interp_host, li, NULL, NULL);
        g_initial_executable_image = NULL;
        g_initial_executable_size = 0;
        *jump = li->entry;
        *at_base = li->base;
        *have_interp = 1;
    }
    // opt8: key the cache by the identity (dev/ino/size/mtime) of the guest binary AND its interpreter,
    // plus the argv[0] basename -- a multicall binary (busybox) runs a different applet per
    // argv[0], and with the exec re-key each image epoch persists its own arena under its own key.
    if (g_pcache)
        g_pc_binid = pcache_make_id(lm->identity, *have_interp ? li->identity : (hl_identity_digest){0}, prog);
    return prog;
}

// W3D: fresh per-launch guest run from a loaded image. Allocates a private heap + stack + cpu and
// runs from `jump`. Shared by standalone/cold and warm-worker paths (which restore a
// pristine COW image first, then calls this against the parent-preloaded base). Body is the original
// common execution tail, including calibration and diagnostic output, so standalone
// behavior is byte-identical.
static int run_loaded(int argc, char *const argv[], struct loaded *lm, uint64_t jump, uint64_t at_base) {
    uint64_t heap;
    uint64_t heap_address = hl_option_get("HL_CHECKPOINT") ? UINT64_C(0x0000050000000000) : 0;
    if (hl_gmap_map_anonymous(heap_address, 256u << 20, HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE,
                              HL_HOST_MEMORY_PRIVATE, &heap) != HL_STATUS_OK)
        return 70;
    brk_lo = brk_cur = heap;
    brk_hi = brk_lo + (256u << 20);

    struct cpu c;
    memset(&c, 0, sizeof c);
    c.fpcw = 0x037f; // x87 default control word (round-to-nearest, all exceptions masked, 64-bit precision)
    c.r[RSP] = build_stack(argc, (char **)argv, lm, at_base); // rsp -> argc
    c.r[RDX] = 0;                                             // rtld_fini = 0
    c.rip = jump;

    proc_reg_publish(g_exe_path, argc, argv); // publish this process into the /proc table
    if (g_untrusted) sentry_init();           // fork the host-authority sentry + (optionally) confine the worker
    thread_process_owner_register(&c);
    run_guest(&c);
    c.exit_code = thread_process_owner_wait(&c, c.exit_code);
    if (g_untrusted) sentry_shutdown(); // signal quit + waitpid (reap, no orphan)
    // Fast-syscall counters are host telemetry, never guest output.  Explicit retained-C diagnostics
    // remain available through the canonical [prof] report emitted by the exit path; normal launches
    // must not synthesize a guest stderr line merely because an inline clock call happened.
    if (g_prof && g_profile_output_owner) {
        char profile[256];
        int profile_size = snprintf(profile, sizeof profile,
                                    "[prof] dispatcher crossings=%llu translations=%llu\n"
                                    "[prof] dispatcher round-trips=%llu  IBTC fills=%llu  (IBTC %s)\n",
                                    (unsigned long long)g_dispatch_profile.crossings,
                                    (unsigned long long)g_dispatch_profile.translations, (unsigned long long)g_disp_n,
                                    (unsigned long long)g_ibtc_fill, "ON");
        if (profile_size > 0) {
            size_t bounded = (size_t)profile_size < sizeof profile ? (size_t)profile_size : sizeof profile - 1u;
            (void)hl_linux_write(g_linux_box, STDERR_FILENO, profile, bounded);
        }
    }
    return c.exit_code;
}

int hl_run_linux_guest(const hl_host_services *host, hl_linux_abi *box, const char *rootfs, hl_host_handle executable,
                       const void *executable_image, size_t executable_size,
                       const hl_executable_authority *executable_authority, const hl_engine_main_image_plan *image_plan,
                       const void *interpreter_image, size_t interpreter_size, uint32_t argument_count,
                       char *const argv[]) {
    int argc;
    g_engine_result_status = HL_STATUS_OK;
    (void)executable;
    g_initial_executable_image = executable_image;
    g_initial_executable_size = executable_size;
    g_initial_interpreter_image = interpreter_image;
    g_initial_interpreter_size = interpreter_size;
    g_authorized_executable_image = executable_image;
    g_authorized_executable_size = executable_size;
    exec_authority_seed_initial(host, executable, executable_authority);
    if (argument_count > (uint32_t)INT_MAX) return 2;
    // The launch argv is copied into build_stack's fixed pointer vector. Nothing between here and there
    // bounded it, so a launch with more than HL_MAXARGV entries wrote past `argp[]` -- observed as
    // "*** stack smashing detected ***" at exactly 2049 entries under the previous 2048 bound, where the
    // host kernel runs the same command without complaint. Reject the launch instead of corrupting the
    // loader's frame.
    if (argument_count > (uint32_t)(HL_MAXARGV - 1)) {
        fprintf(stderr, "hl-engine: guest argument vector exceeds %d entries\n", HL_MAXARGV - 1);
        return 2;
    }
    argc = (int)argument_count;
    hl_target_services_inject(&g_target_services, host);
    hl_gmap_bind_host(host);
    futex_table_init(host);
    seq_ref_arena_init(host);
#if !defined(_WIN32)
    if (namespace_transaction_init(host) != 0) return 1;
#endif
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
        if (hl_host_services_validate(host, HL_HOST_CAP_CLOCK) != HL_STATUS_OK) return hl_vfs_cursor_state_finish(70);
        now = host->clock->monotonic_ns(host->context);
        if (now.status != HL_STATUS_OK) return hl_vfs_cursor_state_finish(70);
        g_host_launch_monotonic_ns = now.value;
    }
    if (bound_shadow_activate() != 0) return hl_vfs_cursor_state_finish(70);
    const char *rdir = hl_option_get("HL_RESTORE");
    if (rdir != NULL) return hl_vfs_cursor_state_finish(ckpt_restore_tree(rootfs));
    if (argc < 1 || !argv || !argv[0]) return hl_vfs_cursor_state_finish(2);
    // Persistent translated-code cache: enabled only by the centralized HL_PCACHE option.
    g_coldprof = 0;
    g_pcache = hl_option_get("HL_PCACHE") != NULL;
    if (container_init(rootfs) != 0) return hl_vfs_cursor_state_finish(70);
    int rc = engine_global_init();
    if (rc) return hl_vfs_cursor_state_finish(rc);
    // Initial-exec shebang handling -- mirror of linux_aarch64.c (and execve case 221) via the shared
    // resolve_shebang_chain(). The container entry may itself be a "#!" script (redis/postgres'
    // docker-entrypoint.sh), and that script's interpreter may ITSELF be a "#!" script (nested, Linux
    // binfmt_script). load_elf has no ELF-magic/#! check, so without this it parses the script text as a
    // bogus ELF (e_machine garbage) and faults before any guest syscall runs. Resolve the whole chain,
    // rewriting argv to [finalInterp, ..., scriptpath, args...] and loading the FINAL interpreter. A
    // non-shebang ELF entry falls straight through unchanged (argc/argv untouched -> byte-identical).
    static char sb_gb[1024], sb_pb[4200], sb_fhb[4200];
    static char sb_store[SHEBANG_MAX * 2][256];
    static char *sb_argv[HL_MAXARGV];
    const char *sb_prog = find_in_path(argv[0], sb_gb, sizeof sb_gb); // bare "sh" -> "/bin/sh" via PATH
    set_guest_comm(sb_prog); // Linux comm = basename of the exec NAME (stays the script's for a shebang entry)
    const char *sb_prog_host = xresolve_overlay(sb_prog, sb_pb, sizeof sb_pb);
    int sb_argc = 0;
    sb_argv[sb_argc++] = (char *)sb_prog;
    for (int i = 1; i < argc && sb_argc < HL_MAXARGV - 1; i++)
        sb_argv[sb_argc++] = (char *)argv[i];
    sb_argv[sb_argc] = NULL;
    const char *sb_finalhost;
    int sb_new =
        resolve_shebang_chain(sb_argv, sb_argc, HL_MAXARGV, sb_prog_host, sb_store, sb_fhb, sizeof sb_fhb, &sb_finalhost);
    if (sb_new < 0) {
        fprintf(stderr, "hl-engine: too many nested #! interpreters (ELOOP): %s\n", argv[0]);
        return hl_vfs_cursor_state_finish(40); // ELOOP
    }
    if (sb_new != sb_argc) { // a shebang chain resolved -> run the final interpreter, not the script
        argc = sb_new;
        argv = (char *const *)sb_argv;
    }
    struct loaded lm, li;
    uint64_t jump, at_base;
    int have_interp;
    /* Calibration selects emitted code, so it must precede cache identity
       construction and lookup. */
    s1_calibrate();
    load_program(argv[0], &lm, &li, &jump, &at_base, &have_interp,
                 image_plan); // (sets g_pc_binid + fixed bases when g_pcache)
    if (g_pcache) {
        g_pc_entry = jump;
        int hit = pcache_load(jump); // graceful MISS on any stale/corrupt/truncated cache -> translate fresh
        if (g_coldprof) fprintf(stderr, "[pcache] %s reloc=%d\n", hit ? "HIT (translation skipped)" : "MISS", g_nreloc);
        if (hl_fatal_status(&g_jit_fatal) != HL_STATUS_OK) {
            g_engine_result_status = hl_fatal_status(&g_jit_fatal);
            pcache_directory_close();
            return hl_vfs_cursor_state_finish(70);
        }
    }
    int ec = run_loaded(argc, argv, &lm, jump, at_base);
    if (hl_fatal_status(&g_jit_fatal) == HL_STATUS_OK)
        pcache_save(); // exit via syscall 93 returns here; syscall 94 saves before _exit (idempotent atomic rename)
    if (hl_fatal_status(&g_jit_fatal) != HL_STATUS_OK) {
        g_engine_result_status = hl_fatal_status(&g_jit_fatal);
        ec = 70;
    }
    pcache_directory_close();
    return hl_vfs_cursor_state_finish(ec);
}

void hl_target_runtime_init(void) {
    jit86_install_sync_fault_guards();
    poslk_init();
    ipc_init();
}

uint64_t hl_run_linux_guest_translations(void) {
    return g_dispatch_profile.translations;
}
