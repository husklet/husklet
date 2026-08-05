#include "run.h"

#include "entry.h"
#include "frontend.h"
#include "projection.h"
#include "word.h"
#include "../../translation.h"

#include <stddef.h>
#include <string.h>

#if defined(__aarch64__) && defined(__linux__)
#include <asm/hwcap.h>
#include <sys/auxv.h>
#elif defined(__aarch64__) && defined(__APPLE__)
#include <sys/sysctl.h>
#elif defined(_M_ARM64)
#include <windows.h>
#endif

#define X86_MAX_BYTES 1024u
#define X86_MAX_WORDS 8192u
#define X86_ENTRY_WORDS 7u
#define X86_RUN_VIEW_COUNT 4u
#define X86_REP_CHUNK_BYTES (UINT64_C(1) << 20)
#define X86_COLD_BUILD_QUOTA 64u

enum x86_fatal_code {
    X86_FATAL_EMPTY_BLOCK = 1,
    X86_FATAL_LOOP_STATE = 2,
    X86_FATAL_BUDGET = 3,
    X86_FATAL_REASON = 4,
};

typedef struct x86_run_views {
    hl_native_projection_view entries[X86_RUN_VIEW_COUNT];
    size_t count;
} x86_run_views;

/* Atomic lowering is never selected from architecture name alone. Unknown
 * platforms and unavailable probes fail closed to the interpreter. */
static int host_has_lse(void) {
#if defined(__aarch64__) && defined(__linux__)
    return (getauxval(AT_HWCAP) & HWCAP_ATOMICS) != 0u;
#elif defined(__aarch64__) && defined(__APPLE__)
    int available = 0;
    size_t size = sizeof available;
    return sysctlbyname("hw.optional.armv8_1_atomics", &available, &size, NULL, 0) == 0 &&
           size == sizeof available && available != 0;
#elif defined(_M_ARM64) && defined(PF_ARM_V81_ATOMIC_INSTRUCTIONS_AVAILABLE)
    return IsProcessorFeaturePresent(PF_ARM_V81_ATOMIC_INSTRUCTIONS_AVAILABLE) != 0;
#else
    return 0;
#endif
}

static int view_contains(const hl_native_projection_view *view, uint64_t address,
                         uint64_t size, uint32_t required) {
    return view != NULL && size != 0 && address <= UINT64_MAX - size &&
           address >= view->guest_first && address + size <= view->guest_last &&
           (view->permissions & required) == required;
}

static void view_promote(x86_run_views *cache, size_t index) {
    hl_native_projection_view selected;
    if (cache == NULL || index >= cache->count || index == 0) return;
    selected = cache->entries[index];
    memmove(&cache->entries[1], &cache->entries[0], index * sizeof(cache->entries[0]));
    cache->entries[0] = selected;
}

static int view_resolve(x86_run_views *cache, hl_native_x86_64_cpu *cpu,
                        uint64_t address, uint64_t size, uint32_t required) {
    if (cache == NULL) return 0;
    for (size_t index = 0; index < cache->count; ++index) {
        hl_native_projection projection;
        if (!view_contains(&cache->entries[index], address, size, required)) continue;
        view_promote(cache, index);
        projection = (hl_native_projection){&cache->entries[0], 1,
                                             cache->entries[0].mapping_incarnation, 0};
        return hl_x86_projection_resolve(&projection, cpu, address, size, required);
    }
    return 0;
}

static void view_install(x86_run_views *cache, const hl_native_projection_view *view) {
    if (cache == NULL || view == NULL) return;
    for (size_t index = 0; index < cache->count; ++index) {
        if (memcmp(&cache->entries[index], view, sizeof(*view)) == 0) {
            view_promote(cache, index);
            return;
        }
    }
    if (cache->count < X86_RUN_VIEW_COUNT) cache->count++;
    if (cache->count > 1)
        memmove(&cache->entries[1], &cache->entries[0],
                (cache->count - 1) * sizeof(cache->entries[0]));
    cache->entries[0] = *view;
}

typedef struct x86_rep {
    uint8_t length;
    uint8_t width;
    uint8_t move;
} x86_rep;

typedef enum x86_rep_result {
    X86_REP_SCALAR = 0,
    X86_REP_COMPLETE = 1,
    X86_REP_EPOCH = 2,
    X86_REP_FATAL = -1,
} x86_rep_result;

static int rep_decode(const uint8_t *bytes, size_t size, x86_rep *rep) {
    uint8_t opcode;
    if (bytes == NULL || rep == NULL || size < 2 || bytes[0] != 0xf3u) return 0;
    if (bytes[1] == 0xa4u || bytes[1] == 0xaau) {
        opcode = bytes[1];
        rep->width = 1u;
        rep->length = 2u;
    } else if (bytes[1] == 0xa5u || bytes[1] == 0xabu) {
        opcode = bytes[1];
        rep->width = 4u;
        rep->length = 2u;
    } else if (size >= 3 && (bytes[1] == 0x66u || bytes[1] == 0x48u) &&
               (bytes[2] == 0xa5u || bytes[2] == 0xabu)) {
        opcode = bytes[2];
        rep->width = bytes[1] == 0x66u ? 2u : 8u;
        rep->length = 3u;
    } else {
        return 0;
    }
    rep->move = opcode == 0xa4u || opcode == 0xa5u;
    return 1;
}

static const hl_native_projection_view *rep_view(const x86_run_views *cache,
                                                  const hl_native_projection *projection,
                                                  uint64_t address, size_t width,
                                                  uint32_t permissions) {
    if (cache != NULL) {
        for (size_t index = 0; index < cache->count; ++index) {
            const hl_native_projection_view *view = &cache->entries[index];
            if (view_contains(view, address, width, permissions)) return view;
        }
    }
    /* The synchronous ProjectionLease authenticates the complete bounded
     * table for the whole run. The four-entry generated-code cache is only a
     * locality accelerator and must not limit bulk REP correctness. */
    if (projection != NULL) {
        for (size_t index = 0; index < projection->count; ++index) {
            const hl_native_projection_view *view = &projection->views[index];
            if (view_contains(view, address, width, permissions)) return view;
        }
    }
    return NULL;
}

static size_t rep_span(const hl_native_projection_view *view, uint64_t address, size_t width,
                       int backward) {
    uint64_t bytes = backward ? address - view->guest_first + width : view->guest_last - address;
    if (bytes > X86_REP_CHUNK_BYTES) bytes = X86_REP_CHUNK_BYTES;
    return (size_t)(bytes - bytes % width);
}

static void rep_copy(void *destination, const void *source, size_t count, size_t width,
                     int backward) {
    uint8_t *dst = destination;
    const uint8_t *src = source;
    size_t bytes = count * width;
    uintptr_t destination_address = (uintptr_t)dst;
    uintptr_t source_address = (uintptr_t)src;
    int disjoint = destination_address <= UINTPTR_MAX - bytes &&
                   source_address <= UINTPTR_MAX - bytes &&
                   (destination_address + bytes <= source_address ||
                    source_address + bytes <= destination_address);
    if (!backward && disjoint) {
        memcpy(dst, src, bytes);
        return;
    }
    for (size_t index = 0; index < count; ++index) {
        uint64_t element = 0;
        const uint8_t *source_element = backward ? src - index * width : src + index * width;
        uint8_t *destination_element = backward ? dst - index * width : dst + index * width;
        memcpy(&element, source_element, width);
        memcpy(destination_element, &element, width);
    }
}

static void rep_fill(void *destination, uint64_t value, size_t count, size_t width,
                     int backward) {
    uint8_t *dst = destination;
    if (!backward && width == 1u) {
        memset(dst, (int)(value & UINT64_C(0xff)), count);
        return;
    }
    for (size_t index = 0; index < count; ++index) {
        uint8_t *element = backward ? dst - index * width : dst + index * width;
        memcpy(element, &value, width);
    }
}

/* Executes only spans already authenticated by the projection lease. Missing
 * views fail closed to the ordinary scalar instruction, which owns resolver
 * callbacks and precise fault exits. */
static x86_rep_result rep_execute(const x86_rep *rep, const x86_run_views *cache,
                                  const hl_native_projection *full_projection,
                                  hl_native_x86_64_cpu *cpu, uint64_t *budget) {
    uint64_t count = cpu->registers[1];
    uint64_t source = cpu->registers[6];
    uint64_t destination = cpu->registers[7];
    int backward = (cpu->flags & (UINT64_C(1) << 10)) != 0;
    const hl_native_projection_view *source_view = NULL;
    const hl_native_projection_view *destination_view;
    size_t source_span = SIZE_MAX;
    size_t destination_span;
    size_t elements;
    uint64_t step;
    if (count == 0) {
        cpu->program += rep->length;
        cpu->executed++;
        --*budget;
        cpu->budget = *budget;
        return X86_REP_COMPLETE;
    }
    if (*budget == 0 || destination > UINT64_MAX - rep->width) return X86_REP_SCALAR;
    destination_view = rep_view(cache, full_projection, destination, rep->width, 2u);
    if (destination_view == NULL) return X86_REP_SCALAR;
    destination_span = rep_span(destination_view, destination, rep->width, backward);
    if (rep->move) {
        if (source > UINT64_MAX - rep->width) return X86_REP_SCALAR;
        source_view = rep_view(cache, full_projection, source, rep->width, 1u);
        if (source_view == NULL) return X86_REP_SCALAR;
        source_span = rep_span(source_view, source, rep->width, backward);
    }
    elements = destination_span / rep->width;
    if (rep->move && elements > source_span / rep->width) elements = source_span / rep->width;
    if ((uint64_t)elements > count) elements = (size_t)count;
    if ((uint64_t)elements > *budget) elements = (size_t)*budget;
    if (elements == 0) return X86_REP_SCALAR;
    hl_native_projection destination_projection = {
        destination_view, 1, destination_view->mapping_incarnation, 0};
    size_t bytes = elements * rep->width;
    uint64_t first = backward ? destination - (bytes - rep->width) : destination;
    if (!hl_x86_projection_switch_writable(cpu, destination_view->guest_first, first + bytes))
        return X86_REP_EPOCH;
    if (!hl_x86_projection_resolve(&destination_projection, cpu, first, bytes, 2u))
        return X86_REP_SCALAR;
    uint8_t *destination_host = (uint8_t *)(uintptr_t)
        (destination_view->host_first + destination - destination_view->guest_first);
    if (rep->move) {
        const uint8_t *source_host = (const uint8_t *)(uintptr_t)
            (source_view->host_first + source - source_view->guest_first);
        rep_copy(destination_host, source_host, elements, rep->width, backward);
    } else {
        rep_fill(destination_host, cpu->registers[0], elements, rep->width, backward);
    }
    /* The preflight above proves this exact post-success bookkeeping call.
     * Failure is internal state corruption, never permission fallback after
     * guest bytes have changed. */
    if (!hl_x86_projection_written(cpu, first, bytes)) return X86_REP_FATAL;
    step = (uint64_t)bytes;
    cpu->registers[7] = backward ? destination - step : destination + step;
    if (rep->move) cpu->registers[6] = backward ? source - step : source + step;
    cpu->registers[1] -= elements;
    cpu->executed += elements;
    *budget -= elements;
    cpu->budget = *budget;
    if (cpu->registers[1] == 0) cpu->program += rep->length;
    return X86_REP_COMPLETE;
}

static void view_publish(const x86_run_views *cache, hl_native_x86_64_cpu *cpu,
                         uint64_t mapping_incarnation) {
    /*
     * The Rust ProjectionLease holds the mapping transaction lock for this
     * entire synchronous run, so an equal nonzero token/incarnation cannot
     * become stale underneath generated code.  Clearing the token at every
     * run boundary rejects prior-run state; publishing it last makes partial
     * entry/count updates invisible to the acquire-side probe.
     */
    size_t count = cache != NULL ? cache->count : 0;
    __atomic_store_n(&cpu->read_token, 0, __ATOMIC_RELEASE);
    cpu->read_count = 0;
    if (count > X86_RUN_VIEW_COUNT) count = X86_RUN_VIEW_COUNT;
    for (size_t index = 0; index < count; ++index) {
        const hl_native_projection_view *view = &cache->entries[index];
        cpu->read_views[index][0] = view->guest_first;
        cpu->read_views[index][1] = view->guest_last;
        cpu->read_views[index][2] = view->host_first - view->guest_first;
        cpu->read_views[index][3] = view->permissions;
    }
    cpu->read_count = count;
    cpu->read_incarnation = mapping_incarnation;
    __atomic_store_n(&cpu->read_token, mapping_incarnation, __ATOMIC_RELEASE);
}

static int source_valid(const hl_native_source *source) {
    size_t index;
    if (source == NULL || source->spans == NULL || source->span_count == 0) return 0;
    for (index = 0; index < source->span_count; ++index) {
        const hl_native_source_span *span = &source->spans[index];
        if (span->bytes == NULL || span->size == 0 || span->guest_first > UINT64_MAX - span->size ||
            span->mapping_incarnation != source->mapping_incarnation ||
            span->instruction_epoch != source->instruction_epoch)
            return 0;
    }
    return 1;
}

static size_t source_bytes(const hl_native_source *source, uint64_t pc, const uint8_t **bytes) {
    size_t index;
    for (index = 0; index < source->span_count; ++index) {
        const hl_native_source_span *span = &source->spans[index];
        uint64_t last = span->guest_first + span->size;
        if (pc >= span->guest_first && pc < last) {
            size_t offset = (size_t)(pc - span->guest_first);
            size_t available = span->size - offset;
            *bytes = span->bytes + offset;
            return available < X86_MAX_BYTES ? available : X86_MAX_BYTES;
        }
    }
    return 0;
}

static hl_native_status leave_exit(hl_native_execution *execution, hl_native_exit *output,
                                   uint32_t kind, uint64_t pc) {
    if (execution->owner != NULL && execution->owner->diagnostics) {
        execution->owner->x86_public_exits++;
        if (kind == HL_NATIVE_EXIT_EPOCH) execution->owner->x86_public_epochs++;
    }
    return hl_native_execution_exit(execution, output, kind, HL_NATIVE_ACCESS_UNKNOWN,
                                    pc, pc, 0, 0);
}

static hl_native_status fatal_exit(hl_native_execution *execution, hl_native_exit *output,
                                   uint64_t code) {
    if (execution->owner != NULL && execution->owner->diagnostics)
        execution->owner->x86_public_exits++;
    return hl_native_execution_exit(execution, output, HL_NATIVE_EXIT_FATAL,
                                    HL_NATIVE_ACCESS_UNKNOWN, 0, 0, 0, code);
}

static hl_native_status target_fallback(hl_native_execution *execution, hl_native_exit *output,
                                        const hl_native_x86_64_cpu *cpu) {
    if (cpu->indirect_site == 0)
        return leave_exit(execution, output, HL_NATIVE_EXIT_FALLBACK, cpu->program);
    if (execution->owner != NULL && execution->owner->diagnostics)
        execution->owner->x86_public_exits++;
    return hl_native_execution_exit(execution, output, HL_NATIVE_EXIT_FALLBACK,
                                    HL_NATIVE_ACCESS_EXECUTE, cpu->indirect_site, cpu->program,
                                    cpu->program, 0);
}

static uint32_t direct_branch(uint32_t source, uint32_t target) {
    int64_t distance = (int64_t)target - (int64_t)source;
    return UINT32_C(0x14000000) | ((uint32_t)distance & UINT32_C(0x03ffffff));
}

_Static_assert(offsetof(hl_native_x86_64_cpu, registers) % (2u * sizeof(uint64_t)) == 0,
               "paired x86 register spills require 16-byte-aligned register storage");
_Static_assert(sizeof(((hl_native_x86_64_cpu *)0)->registers) == 16u * sizeof(uint64_t),
               "paired x86 register spills require sixteen contiguous registers");

static uint32_t store_pair_word(unsigned first, unsigned second, size_t offset) {
    return UINT32_C(0xa9000000) | ((uint32_t)(offset / 8u) & UINT32_C(0x7f)) << 15 |
           second << 10 | 28u << 5 | first;
}

static void spill_registers(uint32_t *words, uint32_t *cursor) {
    for (unsigned index = 0; index < 16u; index += 2u)
        words[(*cursor)++] = store_pair_word(
            index, index + 1u,
            offsetof(hl_native_x86_64_cpu, registers) + index * sizeof(uint64_t));
}

static void finish_execution(uint32_t *words, uint32_t *cursor) {
    words[(*cursor)++] = load_word(16u, offsetof(hl_native_x86_64_cpu, budget));
    words[(*cursor)++] = UINT32_C(0xcb1a0210); /* sub x16,x16,x26 */
    words[(*cursor)++] = store_word(16u, offsetof(hl_native_x86_64_cpu, scratch));
}

static hl_native_status emit_block(hl_native_executor *executor, const hl_native_source *source,
                                   uint64_t pc, const uint8_t *bytes, size_t size,
                                   uint64_t memory_mode, uint64_t authority_generation,
                                   int *supported) {
    uint32_t words[X86_MAX_WORDS];
    hl_x86_a64_provenance frontend_map[HL_X86_A64_MAX_INSTRUCTIONS] = {0};
    hl_native_provenance provenance[HL_X86_A64_MAX_INSTRUCTIONS];
    hl_x86_a64_request request = {
        .abi = HL_X86_A64_FRONTEND_ABI,
        .size = sizeof(request),
        .guest_pc = pc,
        .guest_bytes = bytes,
        .guest_size = size,
        .max_instructions = HL_X86_A64_MAX_INSTRUCTIONS,
        .host_words = words + X86_ENTRY_WORDS,
        .host_capacity = X86_MAX_WORDS - X86_ENTRY_WORDS - 32u,
        .provenance = frontend_map,
        .provenance_capacity = HL_X86_A64_MAX_INSTRUCTIONS,
        .flags = HL_X86_A64_CHECKPOINTS | HL_X86_A64_CONDITIONAL_SELF_LOOP | HL_X86_A64_LIVE_CHAIN |
                 (host_has_lse() ? HL_X86_A64_LSE : 0u) |
                 (executor->diagnostics ? HL_X86_A64_DIAGNOSTICS : 0u),
    };
    hl_x86_a64_result result = {.abi = HL_X86_A64_FRONTEND_ABI, .size = sizeof(result)};
    hl_native_translation_key key;
    hl_native_emission emission;
    hl_native_relocation relocations[2];
    uint32_t relocation_count = 0;
    uint32_t return_sites[3];
    uint32_t return_count = 0;
    uint32_t executable_return = 0;
    uint32_t reason;
    hl_x86_a64_status frontend = hl_x86_a64_emit(&request, &result);
    *supported = 0;
    if (frontend != HL_X86_A64_OK || result.instruction_count == 0 ||
        result.provenance_count != result.instruction_count || result.word_count > X86_MAX_WORDS - 32u ||
        result.source_end <= pc)
        return HL_NATIVE_OK;
    reason = result.exit == HL_X86_A64_SYSCALL ? HL_NATIVE_EXIT_SYSCALL :
             result.exit == HL_X86_A64_INTERPRETER ? HL_NATIVE_EXIT_FALLBACK : HL_NATIVE_EXIT_BRANCH;
    int self_loop = result.exit == HL_X86_A64_CONDITIONAL_BRANCH && result.branch_target == pc;
    words[0] = UINT32_C(0xd503245f);
    words[1] = load_word(26u, offsetof(hl_native_x86_64_cpu, budget));
    words[2] = load_word(16u, offsetof(hl_native_x86_64_cpu, interrupt));
    uint32_t entry_irq = 3;
    uint32_t prefix = 4;
    emit_constant(words, &prefix, 16u, result.instruction_count);
    words[prefix++] = UINT32_C(0xeb10035f); /* cmp x26,x16 */
    uint32_t entry_budget = prefix++;
    if (prefix != X86_ENTRY_WORDS) return HL_NATIVE_STATE;
    uint32_t cursor = result.word_count + X86_ENTRY_WORDS;
    if (self_loop) {
        /* The frontend owns its ordinary taken/fallthrough returns. */
    } else {
        words[cursor++] = load_word(16u, offsetof(hl_native_x86_64_cpu, executable_written));
        executable_return = cursor++;
        emit_constant(words, &cursor, 16u, frontend_map[result.instruction_count - 1u].guest_pc);
        words[cursor++] = store_word(16u, offsetof(hl_native_x86_64_cpu, scratch) + sizeof(uint64_t));
        if (result.exit == HL_X86_A64_DYNAMIC_BRANCH) {
            /* Probe by the architectural target, never by call depth. This is
             * deliberately an IBTC rather than an RSB: recursion, unwinding,
             * and longjmp are ordinary identity misses, not corrupted state. */
            words[cursor++] = UINT32_C(0xd34246b1); /* ubfx x17,x21,#2,#16 */
            emit_constant(words, &cursor, 16u, (uint64_t)(uintptr_t)executor->ibtc);
            words[cursor++] = UINT32_C(0x8b111211); /* add x17,x16,x17,lsl #4 */
            words[cursor++] = UINT32_C(0xa9404632); /* ldp x18,x17,[x17] */
            words[cursor++] = UINT32_C(0xeb15025f); /* cmp x18,x21 */
            uint32_t target_miss = cursor++;
            uint32_t empty_miss = cursor++;
            words[cursor++] = UINT32_C(0xd2800032); /* mov x18,#branch */
            words[cursor++] = store_word(18u, offsetof(hl_native_x86_64_cpu, reason));
            words[cursor++] = store_word(31u, offsetof(hl_native_x86_64_cpu, indirect_site));
            words[cursor++] = UINT32_C(0xd61f0220); /* br x17 */
            uint32_t miss = cursor;
            words[target_miss] = UINT32_C(0x54000000) |
                                 ((miss - target_miss) & UINT32_C(0x7ffff)) << 5 | 1u;
            words[empty_miss] = UINT32_C(0xb4000000) |
                                ((miss - empty_miss) & UINT32_C(0x7ffff)) << 5 | 17u;
        }
        words[cursor++] = UINT32_C(0xd2800000) | ((reason & UINT32_C(0xffff)) << 5) | 16u;
        words[cursor++] = store_word(16, offsetof(hl_native_x86_64_cpu, reason));
        if (result.exit == HL_X86_A64_CONDITIONAL_BRANCH) {
            words[cursor++] = load_word(16u, offsetof(hl_native_x86_64_cpu, program));
            emit_constant(words, &cursor, 17u, result.branch_target);
            words[cursor++] = UINT32_C(0xeb11021f); /* cmp x16,x17 */
            uint32_t taken_branch = cursor++;
            uint32_t fallthrough_tail = cursor++;
            return_sites[return_count++] = fallthrough_tail;
            uint32_t taken_tail = cursor++;
            return_sites[return_count++] = taken_tail;
            words[taken_branch] = UINT32_C(0x54000000) |
                                  ((taken_tail - taken_branch) & UINT32_C(0x7ffff)) << 5;
            relocations[0] = (hl_native_relocation){
                .code_offset = fallthrough_tail * sizeof(uint32_t),
                .target_guest = result.exit_pc,
                .target_instruction_epoch = source->instruction_epoch,
                .target_epoch_known = 1,
                .expected = 0,
            };
            relocations[1] = (hl_native_relocation){
                .code_offset = taken_tail * sizeof(uint32_t),
                .target_guest = result.branch_target,
                .target_instruction_epoch = source->instruction_epoch,
                .target_epoch_known = 1,
                .expected = 0,
            };
            relocation_count = 2;
        } else {
            return_sites[return_count++] = cursor++;
        }
        if (result.exit == HL_X86_A64_DIRECT_BRANCH) {
            relocations[0] = (hl_native_relocation){
                .code_offset = (cursor - 1u) * sizeof(uint32_t),
                .target_guest = result.branch_target,
                .target_instruction_epoch = source->instruction_epoch,
                .target_epoch_known = 1,
                .expected = 0,
            };
            relocation_count = 1;
        }
        if (result.exit == HL_X86_A64_DIRECT_CALL) {
            relocations[0] = (hl_native_relocation){
                .code_offset = (cursor - 1u) * sizeof(uint32_t),
                .target_guest = result.branch_target,
                .target_instruction_epoch = source->instruction_epoch,
                .target_epoch_known = 1,
                .expected = 0,
            };
            relocation_count = 1;
        }
    }
    uint32_t guard_return = cursor;
    finish_execution(words, &cursor);
    spill_registers(words, &cursor);
    words[cursor++] = UINT32_C(0xd65f03c0);
    for (uint32_t index = 0; index < return_count; ++index)
        words[return_sites[index]] = direct_branch(return_sites[index], guard_return);
    if (executable_return != 0)
        words[executable_return] = UINT32_C(0x37000000) | /* tbnz x16,#2,guard_return */
                                   ((guard_return - executable_return) & UINT32_C(0x3fff)) << 5 |
                                   2u << 19 | 16u;
    for (uint32_t index = 0; index < relocation_count; ++index) {
        uint32_t site = (uint32_t)(relocations[index].code_offset / sizeof(uint32_t));
        relocations[index].expected = words[site];
    }
    words[entry_irq] = UINT32_C(0xb5000000) |
                       ((guard_return - entry_irq) & UINT32_C(0x7ffff)) << 5 | 16u;
    words[entry_budget] = UINT32_C(0x54000000) |
                          ((guard_return - entry_budget) & UINT32_C(0x7ffff)) << 5 | 3u;
    uint32_t provenance_count = 0;
    for (uint32_t index = 0; index < result.provenance_count; ++index) {
        if (frontend_map[index].word_end == frontend_map[index].word_start) continue;
        provenance[provenance_count++] = (hl_native_provenance){
            .code_offset = (frontend_map[index].word_start + X86_ENTRY_WORDS) * sizeof(uint32_t),
            .code_size = (frontend_map[index].word_end - frontend_map[index].word_start) * sizeof(uint32_t),
            .guest = frontend_map[index].guest_pc,
        };
    }
    if (provenance_count == 0)
        provenance[provenance_count++] = (hl_native_provenance){
            .code_size = cursor * sizeof(uint32_t), .guest = pc};
    key = (hl_native_translation_key){
        .guest = pc,
        .mapping_incarnation = source->mapping_incarnation,
        .instruction_epoch = source->instruction_epoch,
        .source_first = pc,
        .source_last = result.source_end,
        .memory_mode = memory_mode,
        .authority_generation = authority_generation,
    };
    emission = (hl_native_emission){.bytes = (const uint8_t *)words,
                                    .size = cursor * sizeof(uint32_t),
                                    .body_offset = 2u * sizeof(uint32_t),
                                    .provenance = provenance,
                                    .provenance_count = provenance_count,
                                    .relocations = relocation_count != 0 ? relocations : NULL,
                                    .relocation_count = relocation_count,
                                    .instruction_count = result.instruction_count,
                                    .conditional_self_loop = (uint32_t)self_loop,
                                    .loop_pc = self_loop ? pc : 0,
                                    /* Direct targets enter at word two.  That skips BTI and the
                                     * redundant budget reload, but preserves the interrupt load and
                                     * live x26 budget comparison at every block boundary. */
                                    .cycle_safe = 1};
    hl_native_status status = hl_native_translation_publish(executor, &key, &emission);
    if (status == HL_NATIVE_OK) *supported = 1;
    return status;
}

static hl_native_status execution_enter(hl_native_executor *executor,
                                        hl_native_execution *execution,
                                        hl_native_x86_64_cpu *cpu) {
    hl_native_status status = hl_native_execution_enter(executor, execution);
    if (status != HL_NATIVE_OK) return status;
    status = hl_native_execution_bind_certificate(execution, &cpu->certificate_token);
    if (status != HL_NATIVE_OK) (void)hl_native_execution_leave(execution);
    return status;
}

hl_native_status hl_native_x86_64_run(hl_native_executor *executor, hl_native_x86_64_cpu *cpu,
                                      const hl_native_run_request *request, hl_native_exit *output) {
    const hl_native_source *source = request->source;
    hl_native_source_span resolved_span;
    hl_native_source resolved_source;
    hl_native_source_resolve resolver = NULL;
    void *resolver_context = NULL;
    hl_native_operand_resolve operand_resolver = NULL;
    void *operand_context = NULL;
    x86_run_views operand_views = {0};
    uint64_t budget = request->budget;
    uint64_t cumulative_budget = request->budget;
    hl_native_quantum_poll quantum_poll = NULL;
    void *quantum_context = NULL;
    uint64_t quantum_grant = 0;
    int rep_boundary = 0;
    uint64_t memory_mode = 0;
    uint64_t authority_generation = 0;
    uint64_t authority_identity = 0;
    uint32_t cold_builds = 0;
    const hl_native_direct_token *direct_token = NULL;
    cpu->loop_remaining = 0;
    cpu->loop_completed = 0;
    cpu->loop_block_count = 0;
    cpu->loop_pc = 0;
    cpu->read_token = 0;
    cpu->read_count = 0;
    cpu->vector_dirty = 0;
    if (!source_valid(source) || source->mapping_incarnation != request->mapping_epoch) return HL_NATIVE_ARGUMENT;
    if (request->projection != NULL) {
        const hl_native_projection_view *view = &request->projection->views[request->projection->active];
        if (!hl_x86_projection_validate(request->projection) ||
            request->projection->mapping_incarnation != request->mapping_epoch ||
            !hl_x86_projection_resolve(request->projection, cpu, view->guest_first,
                                       view->guest_last - view->guest_first, view->permissions))
            return HL_NATIVE_ARGUMENT;
        for (size_t index = request->projection->count;
             index > 0 && operand_views.count < X86_RUN_VIEW_COUNT; --index)
            view_install(&operand_views, &request->projection->views[index - 1]);
    }
    view_publish(&operand_views, cpu, request->mapping_epoch);
    if (request->size >= offsetof(hl_native_run_request, direct_token)) {
        memory_mode = request->memory_mode;
        authority_generation = request->authority_generation;
    }
    if (request->size >= offsetof(hl_native_run_request, authority_identity)) direct_token = request->direct_token;
    if (request->size >= sizeof(*request)) authority_identity = request->authority_identity;
    if (request->size >= offsetof(hl_native_run_request, quantum_grant)) {
        quantum_context = request->quantum_context;
        quantum_poll = request->quantum_poll;
    }
    if (request->size >= sizeof(*request)) quantum_grant = request->quantum_grant;
    if ((quantum_poll == NULL) != (quantum_grant == 0)) return HL_NATIVE_ARGUMENT;
    if ((memory_mode == 0) != (authority_generation == 0) ||
        (memory_mode == 0) != (authority_identity == 0)) return HL_NATIVE_ARGUMENT;
    hl_native_status epoch_status = memory_mode == 0
        ? hl_native_synchronize_epoch(executor, source->mapping_incarnation, 0, 0, 0)
        : hl_native_synchronize_direct(executor, source->mapping_incarnation, 0,
                                       direct_token, authority_generation, authority_identity,
                                       request->projection);
    if (epoch_status != HL_NATIVE_OK) return epoch_status;
    if (request->size >= offsetof(hl_native_run_request, operand_context)) {
        resolver = request->source_resolve;
        resolver_context = request->source_context;
    }
    if (request->size >= offsetof(hl_native_run_request, memory_mode)) {
        operand_resolver = request->operand_resolve;
        operand_context = request->operand_context;
    }
    cpu->budget = budget;
    cpu->executed = 0;
    hl_native_execution execution = {0};
    for (;;) {
        hl_native_translation_key lookup;
        hl_native_code code;
        hl_native_status status;
        const uint8_t *bytes = NULL;
        const hl_native_source *active_source = source;
        size_t size;
        int supported;
        uint64_t pc = cpu->program;
        if (execution.owner == NULL) {
            status = execution_enter(executor, &execution, cpu);
            if (status != HL_NATIVE_OK) return status;
            if (memory_mode != 0 && !hl_native_direct_request_valid(
                    executor, direct_token, authority_generation, authority_identity,
                    request->projection)) {
                (void)hl_native_execution_leave(&execution);
                return HL_NATIVE_STATE;
            }
        }
        if (cpu->interrupt != 0) return leave_exit(&execution, output, HL_NATIVE_EXIT_INTERRUPT, pc);
        if (budget == 0) {
            if (!rep_boundary || quantum_poll == NULL ||
                !quantum_poll(quantum_context, cpu->executed, cumulative_budget))
                return leave_exit(&execution, output, HL_NATIVE_EXIT_YIELD, pc);
            if (cumulative_budget > UINT64_MAX - quantum_grant ||
                cpu->executed > cumulative_budget)
                return fatal_exit(&execution, output, X86_FATAL_BUDGET);
            cumulative_budget += quantum_grant;
            budget = quantum_grant;
            cpu->budget = budget;
            rep_boundary = 0;
        }
        size = source_bytes(source, pc, &bytes);
        x86_rep rep;
        x86_rep_result rep_result = size != 0 && rep_decode(bytes, size, &rep)
            ? rep_execute(&rep, &operand_views, request->projection, cpu, &budget)
            : X86_REP_SCALAR;
        if (rep_result == X86_REP_FATAL) return fatal_exit(&execution, output, X86_FATAL_REASON);
        if (rep_result == X86_REP_EPOCH) return leave_exit(&execution, output, HL_NATIVE_EXIT_EPOCH, pc);
        if (rep_result == X86_REP_COMPLETE) {
            /* Both an incomplete REP and its completed architectural boundary
             * have fully published RCX/RSI/RDI and exact write bookkeeping.
             * Let the generation-qualified poll admit the following quantum
             * here so exact-quantum completion does not force a public run
             * round-trip before the next instruction. */
            rep_boundary = budget == 0;
            if ((cpu->executable_written & 4u) != 0)
                return leave_exit(&execution, output, HL_NATIVE_EXIT_EPOCH, cpu->program);
            continue;
        }
        lookup = (hl_native_translation_key){
            .guest = pc,
            .mapping_incarnation = source->mapping_incarnation,
            .instruction_epoch = source->instruction_epoch,
            .source_first = pc,
            .source_last = pc + 1,
            .memory_mode = memory_mode,
            .authority_generation = memory_mode != 0 ? authority_identity : authority_generation,
        };
        if (hl_native_translation_lookup(executor, &lookup, &code) != HL_NATIVE_HIT) {
            status = hl_native_execution_leave(&execution);
            if (status != HL_NATIVE_OK) return status;
            size = source_bytes(source, pc, &bytes);
            if (size == 0 && resolver != NULL) {
                memset(&resolved_span, 0, sizeof(resolved_span));
                if (resolver(resolver_context, pc, source->mapping_incarnation,
                             source->instruction_epoch, &resolved_span)) {
                    resolved_source = (hl_native_source){&resolved_span, 1, source->mapping_incarnation,
                                                         resolved_span.instruction_epoch};
                    if (!source_valid(&resolved_source)) return HL_NATIVE_ARGUMENT;
                    active_source = &resolved_source;
                    size = source_bytes(&resolved_source, pc, &bytes);
                }
            }
            lookup.instruction_epoch = active_source->instruction_epoch;
            if (size != 0 && hl_native_translation_lookup(executor, &lookup, &code) == HL_NATIVE_HIT) {
                supported = 1;
            } else if (size != 0) {
                if (cold_builds == X86_COLD_BUILD_QUOTA) {
                    if (executor->diagnostics)
                        atomic_fetch_add_explicit(&executor->x86_cold_quota_exits, 1, memory_order_relaxed);
                    if ((status = execution_enter(executor, &execution, cpu)) != HL_NATIVE_OK) return status;
                    return leave_exit(&execution, output, HL_NATIVE_EXIT_YIELD, pc);
                }
                cold_builds++;
                status = emit_block(executor, active_source, pc, bytes, size,
                                    memory_mode, memory_mode != 0 ? authority_identity : authority_generation,
                                    &supported);
                if (executor->diagnostics) {
                    atomic_fetch_add_explicit(&executor->x86_cold_builds, 1, memory_order_relaxed);
                }
            }
            if (size != 0 && status != HL_NATIVE_OK) return status;
            if (size == 0 || !supported) {
                if ((status = execution_enter(executor, &execution, cpu)) != HL_NATIVE_OK) return status;
                if (size == 0) return target_fallback(&execution, output, cpu);
                return leave_exit(&execution, output, HL_NATIVE_EXIT_FALLBACK, pc);
            }
            if ((status = execution_enter(executor, &execution, cpu)) != HL_NATIVE_OK) return status;
            if (hl_native_translation_lookup(executor, &lookup, &code) != HL_NATIVE_HIT)
                return leave_exit(&execution, output, HL_NATIVE_EXIT_EPOCH, pc);
        }
        if (code.instruction_count == 0)
            return fatal_exit(&execution, output, X86_FATAL_EMPTY_BLOCK);
        if (budget < code.instruction_count) return leave_exit(&execution, output, HL_NATIVE_EXIT_YIELD, pc);
        if (cpu->indirect_site != 0) {
            if (executor->diagnostics) {
                atomic_fetch_add_explicit(&executor->ibtc_site_misses, 1, memory_order_relaxed);
                atomic_fetch_add_explicit(&executor->ibtc_shared_misses, 1, memory_order_relaxed);
            }
            if (code.conditional_self_loop == 0)
                hl_native_ibtc_fill_shared(executor, pc, code.body);
            cpu->indirect_site = 0;
        }
        int loop_active = code.conditional_self_loop != 0 && code.identity_token != 0 &&
                          code.mapping_epoch == source->mapping_incarnation &&
                          code.instruction_epoch == lookup.instruction_epoch && code.loop_pc == pc &&
                          code.source_first == pc && code.source_last > pc;
        if (loop_active) {
            uint64_t iterations = budget / code.instruction_count;
            if (iterations > 256) iterations = 256;
            cpu->loop_remaining = iterations;
            cpu->loop_completed = 0;
            cpu->loop_block_count = code.instruction_count;
            cpu->loop_pc = pc;
        }
        cpu->scratch[0] = 0;
        cpu->fault_address = 0;
        cpu->fault_access = 0;
        cpu->fault_size = 0;
        uint64_t executed_before = cpu->executed;
        hl_native_x86_64_enter(cpu, code.entry);
        uint64_t vector_dirty = cpu->vector_dirty;
        cpu->vector_dirty = 0;
        uint64_t completed = cpu->scratch[0];
        if (loop_active) {
            if (cpu->loop_block_count != code.instruction_count || cpu->loop_pc != pc ||
                cpu->loop_completed > UINT64_MAX / code.instruction_count) {
                cpu->loop_remaining = cpu->loop_completed = cpu->loop_block_count = cpu->loop_pc = 0;
                return fatal_exit(&execution, output, X86_FATAL_LOOP_STATE);
            }
            completed = cpu->loop_completed * code.instruction_count;
            if (cpu->reason == HL_NATIVE_EXIT_FALLBACK) {
                if (cpu->scratch[0] > UINT64_MAX - completed) {
                    cpu->loop_remaining = cpu->loop_completed = cpu->loop_block_count =
                        cpu->loop_pc = 0;
                    return fatal_exit(&execution, output, X86_FATAL_LOOP_STATE);
                }
                completed += cpu->scratch[0];
            }
            cpu->loop_remaining = cpu->loop_completed = cpu->loop_block_count = cpu->loop_pc = 0;
        }
        if (completed > budget) return fatal_exit(&execution, output, X86_FATAL_BUDGET);
        budget -= completed;
        cpu->budget = budget;
        cpu->executed += completed;
        if (executor->diagnostics) {
            executor->completed += cpu->executed - executed_before;
            switch (cpu->reason) {
                case HL_NATIVE_EXIT_BRANCH: executor->boundary_branch++; break;
                case HL_NATIVE_EXIT_SYSCALL: executor->boundary_syscall++; break;
                case HL_NATIVE_EXIT_FALLBACK: executor->boundary_fallback++; break;
                case HL_NATIVE_EXIT_YIELD: executor->boundary_yield++; break;
                default: break;
            }
        }
        if ((cpu->executable_written & 4u) != 0)
            return leave_exit(&execution, output, HL_NATIVE_EXIT_EPOCH, cpu->program);
        if (cpu->interrupt != 0)
            return leave_exit(&execution, output, HL_NATIVE_EXIT_INTERRUPT, cpu->program);
        if (loop_active && cpu->reason != HL_NATIVE_EXIT_FALLBACK) {
            continue;
        }
        if (cpu->reason == HL_NATIVE_EXIT_FALLBACK && cpu->fault_access != 0 &&
            cpu->fault_size != 0 && operand_resolver != NULL) {
            hl_native_projection_view view = {0};
            hl_native_projection resolved_projection;
            uint32_t result;
            if (view_resolve(&operand_views, cpu, cpu->fault_address, cpu->fault_size,
                             (uint32_t)cpu->fault_access)) {
                if (executor->diagnostics) executor->operand_cache_hits++;
                continue;
            }
            status = hl_native_execution_leave(&execution);
            if (status != HL_NATIVE_OK) return status;
            if (cpu->interrupt != 0 || budget == 0) continue;
            if (executor->diagnostics) executor->operand_callbacks++;
            result = operand_resolver(operand_context, cpu->fault_address, cpu->fault_size,
                                      (uint32_t)cpu->fault_access, source->mapping_incarnation,
                                      source->instruction_epoch, &view);
            if (result == HL_NATIVE_OPERAND_RESOLVED) {
                resolved_projection = (hl_native_projection){&view, 1, source->mapping_incarnation, 0};
                if (!hl_x86_projection_validate(&resolved_projection) ||
                    !hl_x86_projection_resolve(&resolved_projection, cpu, cpu->fault_address,
                                               cpu->fault_size, (uint32_t)cpu->fault_access))
                    return HL_NATIVE_ARGUMENT;
                view_install(&operand_views, &view);
                view_publish(&operand_views, cpu, source->mapping_incarnation);
                cpu->reason = 0;
                continue;
            }
            if ((status = execution_enter(executor, &execution, cpu)) != HL_NATIVE_OK) return status;
            if (result == HL_NATIVE_OPERAND_FAULT)
            {
                if (execution.owner != NULL && execution.owner->diagnostics)
                    execution.owner->x86_public_exits++;
                return hl_native_execution_exit(&execution, output, HL_NATIVE_EXIT_FAULT,
                                                (uint32_t)cpu->fault_access, cpu->program, cpu->program,
                                                cpu->fault_address, 1);
            }
            if (result == HL_NATIVE_OPERAND_EPOCH)
                return leave_exit(&execution, output, HL_NATIVE_EXIT_EPOCH, cpu->program);
            if (result != HL_NATIVE_OPERAND_DECLINED) {
                status = hl_native_execution_leave(&execution);
                return status == HL_NATIVE_OK ? HL_NATIVE_ARGUMENT : status;
            }
            return leave_exit(&execution, output, HL_NATIVE_EXIT_FALLBACK, cpu->program);
        }
        if (cpu->reason == HL_NATIVE_EXIT_BRANCH) {
            continue;
        }
        if (cpu->reason == HL_NATIVE_EXIT_SYSCALL) {
            if (executor->diagnostics) {
                executor->x86_public_exits++;
                executor->x86_public_syscalls++;
                if (vector_dirty != 0) executor->x86_syscall_vector_dirty++;
            }
            return hl_native_execution_exit(&execution, output, HL_NATIVE_EXIT_SYSCALL,
                                            HL_NATIVE_ACCESS_UNKNOWN, cpu->scratch[1], cpu->program, 0, 0);
        }
        if (cpu->reason == HL_NATIVE_EXIT_FALLBACK)
            return leave_exit(&execution, output, HL_NATIVE_EXIT_FALLBACK, cpu->program);
        return fatal_exit(&execution, output, X86_FATAL_REASON);
    }
}
