#include "trace.h"

#include "add.h"
#include "arithmetic.h"
#include "bitwise.h"
#include "broadcast.h"
#include "compare.h"
#include "conditional.h"
#include "crypto.h"
#include "direct.h"
#include "divide.h"
#include "field.h"
#include "fp_move.h"
#include "floating.h"
#include "logical.h"
#include "indirect.h"
#include "move.h"
#include "multiply.h"
#include "memory.h"
#include "ordered.h"
#include "pair_arithmetic.h"
#include "pcrel.h"
#include "reverse.h"
#include "pair.h"
#include "select.h"
#include "shift.h"
#include "simd_compare.h"
#include "simd_difference.h"
#include "fp_reduce.h"
#include "simd_immediate.h"
#include "simd_integer.h"
#include "simd_narrow.h"
#include "simd_pairwise.h"
#include "simd_permute.h"
#include "simd_reduce.h"
#include "simd_reciprocal.h"
#include "simd_saturating.h"
#include "simd_widening.h"
#include "single.h"
#include "structure.h"
#include "system.h"
#include "stub.h"
#include "zero.h"

#include <string.h>

static void density_add(uint64_t *value, uint64_t add, hl_a64_trace_density *density) {
    if (UINT64_MAX - *value < add) {
        *value = UINT64_MAX;
        density->saturated = 1;
    } else {
        *value += add;
    }
}

static void density_record(hl_a64_trace_density *density, hl_a64_density_family family,
                           size_t words, int instruction) {
    if (density == NULL) return;
    if (instruction) density_add(&density->families[family].guest_instructions, 1, density);
    density_add(&density->families[family].hot_words, words, density);
}

static hl_a64_density_family density_family(uint32_t word, uint64_t pc,
                                            const hl_a64_memory_sites *sites) {
    hl_a64_memory memory;
    if (sites->count == 0) return HL_A64_DENSITY_OTHER;
    if (hl_a64_memory_decode(word, pc, &memory) && memory.kind == HL_A64_MEMORY_PAIR)
        return memory.read ? HL_A64_DENSITY_PAIR_READ : HL_A64_DENSITY_PAIR_WRITE;
    return HL_A64_DENSITY_SCALAR_MEMORY;
}

static int append_unknown(hl_a64_trace_result *output, size_t first, size_t last, uint64_t guest) {
    if (last <= first || output->provenance_count >= HL_NATIVE_PROVENANCE_MAX) return 0;
    output->provenance[output->provenance_count++] = (hl_native_provenance){
        .code_offset = first, .code_size = last - first, .guest = guest};
    return 1;
}

static int append_memory(hl_a64_trace_result *output, const hl_a64_memory_sites *sites,
                         uint64_t guest) {
    if (sites->count == 0 || sites->count > HL_A64_MEMORY_SITE_MAX ||
        sites->count > HL_NATIVE_PROVENANCE_MAX - output->provenance_count)
        return 0;
    for (uint32_t index = 0; index < sites->count; ++index) {
        const hl_a64_memory_site *site = &sites->entries[index];
        if (site->access != HL_NATIVE_ACCESS_READ && site->access != HL_NATIVE_ACCESS_WRITE) return 0;
        output->provenance[output->provenance_count++] = (hl_native_provenance){
            .code_offset = site->code_offset,
            .code_size = sizeof(uint32_t),
            .guest = guest,
            .address = {
                .displacement = site->displacement,
                .kind = HL_NATIVE_ADDRESS_BASE,
                .bits = 64,
                .base = 16,
            },
            .access = site->access,
            .width = site->width,
        };
    }
    return 1;
}

static int hint_body(hl_a64_assembler *assembler, uint32_t word) {
    if ((word & 0xFFFFF01Fu) != 0xD503201Fu) return 0;
    /* EL0 hints are architecturally optional. In particular, executing a
     * guest PACIASP/AUTIASP against the native stub's host link register
     * would sign or authenticate engine state rather than guest state. */
    hl_a64_emit32(assembler, 0xD503201Fu);
    return 1;
}

static int body(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    return hint_body(assembler, word) || hl_a64_move_body(assembler, word) || hl_a64_add_body(assembler, word) ||
           hl_a64_logical_body(assembler, word) || hl_a64_select_body(assembler, word) ||
           hl_a64_arithmetic_body(assembler, word) || hl_a64_bitwise_body(assembler, word) ||
           hl_a64_crypto_body(assembler, word) ||
           hl_a64_broadcast_body(assembler, word) ||
           hl_a64_fp_move_body(assembler, word) ||
           hl_a64_floating_body(assembler, word) ||
           hl_a64_pcrel_body(assembler, word, pc) || hl_a64_field_body(assembler, word) ||
           hl_a64_multiply_body(assembler, word) || hl_a64_divide_body(assembler, word) ||
           hl_a64_pair_arithmetic_body(assembler, word) ||
           hl_a64_shift_body(assembler, word) || hl_a64_reverse_body(assembler, word) ||
           hl_a64_simd_compare_body(assembler, word) || hl_a64_compare_body(assembler, word) ||
           hl_a64_simd_difference_body(assembler, word) ||
           hl_a64_simd_fp_reduce_body(assembler, word) ||
           hl_a64_simd_integer_body(assembler, word) ||
           hl_a64_simd_narrow_body(assembler, word) ||
           hl_a64_simd_pairwise_body(assembler, word) ||
           hl_a64_simd_permute_body(assembler, word) ||
           hl_a64_simd_reduce_body(assembler, word) ||
           hl_a64_simd_reciprocal_body(assembler, word) ||
           hl_a64_simd_saturating_body(assembler, word) ||
           hl_a64_simd_widening_body(assembler, word) ||
           hl_a64_system_body(assembler, word);
}

static uint32_t register_definition(unsigned reg) {
    return UINT32_C(1) << reg;
}

static int64_t loop_sign_extend(uint64_t value, unsigned bits) {
    uint64_t sign = UINT64_C(1) << (bits - 1);
    return (int64_t)((value ^ sign) - sign);
}

static int add_signed_checked(uint64_t base, int64_t offset, uint64_t *result);

static int body_accepts(uint32_t word, uint64_t pc, hl_a64_memory *memory) {
    uint8_t storage[HL_A64_TRACE_MAX_BYTES];
    hl_a64_assembler assembler;
    hl_a64_guard guard = {0};
    hl_a64_memory_sites sites;
    if (!hl_a64_assembler_begin(&assembler, storage, storage, sizeof(storage))) return 0;
    if (body(&assembler, word, pc) ||
        hl_a64_single_direct_body(&assembler, word, pc, NULL, &guard, &sites) ||
        hl_a64_zero_body(&assembler, word, pc, &guard, &sites) ||
        hl_a64_ordered_body(&assembler, word, pc, &guard, &sites) ||
        hl_a64_simd_immediate_body(&assembler, word) ||
        hl_a64_single_body(&assembler, word, pc, &guard, &sites) ||
        hl_a64_pair_body(&assembler, word, pc, &guard, &sites) ||
        hl_a64_structure_body(&assembler, word, pc, &guard, &sites)) {
        if (memory != NULL) hl_a64_memory_decode(word, pc, memory);
        return hl_a64_assembler_ok(&assembler);
    }
    return 0;
}

hl_a64_instruction_effect hl_a64_trace_effect(uint32_t word, uint64_t pc) {
    hl_a64_instruction_effect effect = {UINT32_MAX, 1, 0, {0, 0}};
    hl_a64_memory memory = {0};
    unsigned rd = word & 31u;
    int indirect_branch = (word & 0xfffffc1fu) == 0xd61f0000u;
    int indirect_call = (word & 0xfffffc1fu) == 0xd63f0000u;
    int indirect_return = (word & 0xfffffc1fu) == 0xd65f0000u;

    /* All control transfers terminate a certifiable straight-line range. */
    if ((word & 0x7c000000u) == 0x14000000u) {
        effect.gpr_definitions = (word & 0x80000000u) ? register_definition(30) : 0;
        effect.control = 1;
        return effect;
    }
    if ((word & 0xff000010u) == 0x54000000u ||
        (word & 0x7e000000u) == 0x34000000u ||
        (word & 0x7e000000u) == 0x36000000u || indirect_branch || indirect_call || indirect_return) {
        effect.gpr_definitions = indirect_call ? register_definition(30) : 0;
        effect.control = 1;
        return effect;
    }
    if ((word & 0xffe0001fu) == 0xd4000001u) return effect;
    if (!body_accepts(word, pc, &memory)) return effect;

    effect.terminal = 0;
    effect.gpr_definitions = 0;
    if (memory.kind != HL_A64_MEMORY_NONE) {
        if (memory.read && !memory.vector && memory.kind != HL_A64_MEMORY_PREFETCH) {
            if (memory.target != 31) effect.gpr_definitions |= register_definition(memory.target);
            if (memory.kind == HL_A64_MEMORY_PAIR && memory.target2 != 31)
                effect.gpr_definitions |= register_definition(memory.target2);
        }
        if (memory.writeback) effect.gpr_definitions |= register_definition(memory.base);
        /* Exclusive/atomic encodings have additional result operands whose
         * roles vary by subfamily.  Preserve safety until those are split. */
        if (memory.kind == HL_A64_MEMORY_EXCLUSIVE || memory.kind == HL_A64_MEMORY_ATOMIC)
            effect.gpr_definitions = UINT32_MAX;
        return effect;
    }

    /* Integer and PC-relative body families consistently place their result
     * in Rd.  Rd==31 is XZR for these families, not SP. */
    if ((word & 0x1f000000u) == 0x10000000u ||
        (word & 0x1f000000u) == 0x11000000u ||
        (word & 0x1f800000u) == 0x12000000u ||
        (word & 0x1f800000u) == 0x12800000u ||
        (word & 0x1f800000u) == 0x13000000u ||
        (word & 0x1f200000u) == 0x0b000000u ||
        (word & 0x1f200000u) == 0x0b200000u ||
        (word & 0x1f000000u) == 0x0a000000u ||
        (word & 0x1f000000u) == 0x1b000000u ||
        (word & 0x1fe00000u) == 0x1a000000u ||
        (word & 0x5fe00000u) == 0x5ac00000u ||
        (word & 0x1fe0f000u) == 0x1ac02000u ||
        (word & 0x1fe0f800u) == 0x1ac00800u ||
        (word & 0x3fe00800u) == 0x1a800000u) {
        if (rd != 31)
            effect.gpr_definitions = register_definition(rd);
        else if (((word & 0x1f000000u) == 0x11000000u ||
                  (word & 0x1f200000u) == 0x0b200000u) &&
                 (word & (1u << 29)) == 0)
            /* ADD/SUB immediate and extended-register permit SP as Rd only
             * when they do not set flags.  Their flag-setting aliases use ZR. */
            effect.gpr_definitions = register_definition(31);
        return effect;
    }

    /* System-register reads use Rt.  Hints have no architectural GPR result.
     * Other accepted FP/SIMD/system families stay deliberately imprecise:
     * some FMOV forms cross the vector/GPR boundary. */
    if ((word & 0xfff00000u) == 0xd5300000u && rd != 31)
        effect.gpr_definitions = register_definition(rd);
    else if ((word & 0xfffff01fu) != 0xd503201fu)
        effect.gpr_definitions = UINT32_MAX;
    return effect;
}

static int self_add_immediate(uint32_t word, unsigned *reg, uint64_t *value, int subtract_flags) {
    uint32_t opcode = word & UINT32_C(0xffc00000);
    uint32_t expected = subtract_flags ? UINT32_C(0xf1000000) : UINT32_C(0x91000000);
    unsigned rd = word & 31u, rn = (word >> 5) & 31u;
    if (opcode != expected || rd != rn || rd == 31 || ((word >> 22) & 1u) != 0) return 0;
    *reg = rd;
    *value = (word >> 10) & UINT32_C(0xfff);
    return *value != 0;
}

int hl_a64_trace_loop_plan(const uint32_t *words, size_t count, uint64_t pc,
                           hl_a64_loop_plan *output) {
    hl_a64_loop_plan plan = {0};
    uint8_t closed[2] = {0};
    uint8_t write_seen[2] = {0};
    int64_t write_first[2] = {0};
    int64_t write_cursor[2] = {0};
    uint32_t memory_definitions = 0;
    if (output == NULL) return 0;
    memset(output, 0, sizeof(*output));
    if (words == NULL || count < 4 || count > HL_A64_TRACE_MAX_WORDS ||
        pc > UINT64_MAX - (count - 1) * UINT64_C(4)) return 0;
    uint32_t branch_word = words[count - 1];
    if ((branch_word & UINT32_C(0xff00001f)) != UINT32_C(0x54000008)) return 0; /* B.HI */
    int64_t displacement = loop_sign_extend((branch_word >> 5) & UINT32_C(0x7ffff), 19) * 4;
    uint64_t branch_pc = pc + (count - 1) * UINT64_C(4), target;
    if (!add_signed_checked(branch_pc, displacement, &target) || target != pc) return 0;

    for (size_t index = 0; index + 1 < count; ++index) {
        hl_a64_memory memory;
        unsigned reg;
        uint64_t value;
        uint64_t instruction = pc + index * 4;
        if (hl_a64_memory_decode(words[index], instruction, &memory)) {
            hl_a64_instruction_effect effect = hl_a64_trace_effect(words[index], instruction);
            if (memory.writeback || memory.kind == HL_A64_MEMORY_LITERAL ||
                memory.kind == HL_A64_MEMORY_REGISTER || memory.kind == HL_A64_MEMORY_EXCLUSIVE ||
                memory.kind == HL_A64_MEMORY_ATOMIC || memory.kind == HL_A64_MEMORY_STRUCTURE ||
                memory.kind == HL_A64_MEMORY_PREFETCH ||
                (memory.kind != HL_A64_MEMORY_PAIR && memory.kind != HL_A64_MEMORY_UNSIGNED &&
                 memory.kind != HL_A64_MEMORY_UNSCALED) || memory.bytes == 0 ||
                memory.bytes > INT64_MAX || memory.offset > INT64_MAX - (int64_t)memory.bytes)
                return 0;
            if (effect.terminal || effect.control || effect.gpr_definitions == UINT32_MAX) return 0;
            memory_definitions |= effect.gpr_definitions;
            unsigned range = 0;
            while (range < plan.range_count && plan.ranges[range].base != memory.base) range++;
            if (range == plan.range_count) {
                if (plan.range_count == 2) return 0;
                plan.ranges[range].base = memory.base;
                plan.ranges[range].minimum = memory.offset;
                plan.ranges[range].maximum = memory.offset + (int64_t)memory.bytes;
                plan.range_count++;
            } else if (closed[range]) return 0;
            if (memory.offset < plan.ranges[range].minimum) plan.ranges[range].minimum = memory.offset;
            if (memory.offset + (int64_t)memory.bytes > plan.ranges[range].maximum)
                plan.ranges[range].maximum = memory.offset + (int64_t)memory.bytes;
            if (memory.write) {
                if (!write_seen[range]) {
                    write_seen[range] = 1;
                    write_first[range] = memory.offset;
                } else if (memory.offset != write_cursor[range]) {
                    return 0;
                }
                write_cursor[range] = memory.offset + (int64_t)memory.bytes;
            }
            plan.ranges[range].permissions |= memory.read ? HL_NATIVE_ACCESS_READ : 0;
            plan.ranges[range].permissions |= memory.write ? HL_NATIVE_ACCESS_WRITE : 0;
            plan.memory_count++;
            continue;
        }
        if (self_add_immediate(words[index], &reg, &value, 0)) {
            unsigned range = 0;
            while (range < plan.range_count && plan.ranges[range].base != reg) range++;
            if (range == plan.range_count || closed[range] || plan.ranges[range].step != 0) return 0;
            plan.ranges[range].step = value;
            closed[range] = 1;
            continue;
        }
        if (index + 2 == count && self_add_immediate(words[index], &reg, &value, 1)) {
            plan.trip = (uint8_t)reg;
            plan.decrement = value;
            continue;
        }
        return 0;
    }
    if (plan.memory_count < 2 || plan.range_count == 0 || plan.trip == 31 || plan.decrement == 0)
        return 0;
    for (unsigned range = 0; range < plan.range_count; ++range) {
        if (!closed[range] || plan.ranges[range].step == 0 || plan.ranges[range].base == plan.trip)
            return 0;
        if ((plan.ranges[range].permissions & HL_NATIVE_ACCESS_WRITE) != 0) {
            if (!write_seen[range] || write_cursor[range] <= write_first[range] ||
                (uint64_t)(write_cursor[range] - write_first[range]) != plan.ranges[range].step)
                return 0;
            plan.ranges[range].writes_contiguous = 1;
        }
    }
    if ((memory_definitions & register_definition(plan.trip)) != 0) return 0;
    for (unsigned range = 0; range < plan.range_count; ++range)
        if ((memory_definitions & register_definition(plan.ranges[range].base)) != 0) return 0;
    plan.loop_pc = pc;
    plan.instruction_count = (uint32_t)count;
    *output = plan;
    return 1;
}

hl_a64_loop_preflight_status hl_a64_trace_loop_preflight(const hl_native_aarch64_cpu *cpu,
    const hl_a64_loop_plan *plan, uint64_t mapping_incarnation, uint64_t authority,
    uint64_t budget, hl_a64_loop_entry *output) {
    hl_a64_loop_entry entry = {0};
    if (output == NULL) return HL_A64_LOOP_PREFLIGHT_REJECT;
    memset(output, 0, sizeof(*output));
    if (cpu == NULL || plan == NULL || mapping_incarnation == 0 || authority == 0 ||
        cpu->active_authority != authority || plan->range_count == 0 || plan->range_count > 2 ||
        plan->trip >= 31 || plan->decrement == 0 || plan->instruction_count == 0 ||
        cpu->read_count == 0 || cpu->read_count > 4)
        return HL_A64_LOOP_PREFLIGHT_REJECT;
    uint64_t token = __atomic_load_n(&cpu->read_token, __ATOMIC_ACQUIRE);
    if (token != mapping_incarnation || cpu->read_incarnation != token) return HL_A64_LOOP_PREFLIGHT_REJECT;
    entry.trip = cpu->registers[plan->trip];
    entry.decrement = plan->decrement;
    entry.instruction_count = plan->instruction_count;
    entry.iterations = entry.trip / entry.decrement + (entry.trip % entry.decrement != 0);
    if (entry.iterations == 0 || entry.iterations > UINT64_MAX / entry.instruction_count)
        return HL_A64_LOOP_PREFLIGHT_REJECT;
    entry.budget_iterations = budget / entry.instruction_count;
    if (entry.budget_iterations == 0) return HL_A64_LOOP_PREFLIGHT_REJECT;
    if (entry.budget_iterations > entry.iterations) entry.budget_iterations = entry.iterations;
    entry.mapping_incarnation = mapping_incarnation;
    entry.authority = authority;

    uint32_t write_ranges = 0;
    for (uint32_t range = 0; range < plan->range_count; ++range) {
        const hl_a64_loop_range *source = &plan->ranges[range];
        uint64_t advance, last_base, first, last;
        if (source->base >= 31 || source->minimum >= source->maximum || source->step == 0 ||
            (entry.iterations - 1) > UINT64_MAX / source->step)
            return HL_A64_LOOP_PREFLIGHT_REJECT;
        advance = (entry.iterations - 1) * source->step;
        if (cpu->registers[source->base] > UINT64_MAX - advance) return HL_A64_LOOP_PREFLIGHT_REJECT;
        last_base = cpu->registers[source->base] + advance;
        if (!add_signed_checked(cpu->registers[source->base], source->minimum, &first) ||
            !add_signed_checked(last_base, source->maximum, &last) || last <= first)
            return HL_A64_LOOP_PREFLIGHT_REJECT;
        const uint64_t *view = NULL;
        for (uint64_t index = 0; index < cpu->read_count; ++index) {
            const uint64_t *candidate = cpu->read_views[index];
            if (candidate[1] > candidate[0] && candidate[3] != 0 && (candidate[3] & ~UINT64_C(7)) == 0 &&
                first >= candidate[0] && last <= candidate[1] &&
                (candidate[3] & source->permissions) == source->permissions) {
                view = candidate;
                break;
            }
        }
        if (view == NULL) return HL_A64_LOOP_PREFLIGHT_REJECT;
        /* Slots preserve plan-range order even when both envelopes share one
         * physical view, so future emitted members need no ambiguous lookup. */
        entry.views[range][0] = first;
        entry.views[range][1] = last;
        entry.views[range][2] = view[0];
        entry.views[range][3] = view[1];
        entry.views[range][4] = view[2];
        entry.views[range][5] = view[3];
        entry.view_count = range + 1;
        if ((source->permissions & HL_NATIVE_ACCESS_WRITE) != 0) {
            if (++write_ranges > 1 || !source->writes_contiguous) return HL_A64_LOOP_PREFLIGHT_REJECT;
            if ((view[3] & UINT64_C(4)) != 0) entry.executable = 1;
            /* With no reserved journal slot, even a presently adjacent
             * envelope may fault after changing the active owner. */
            if (cpu->dirty_first != UINT64_MAX && cpu->dirty_count >= 16)
                return HL_A64_LOOP_PREFLIGHT_EPOCH;
        }
    }
    *output = entry;
    return entry.executable ? HL_A64_LOOP_PREFLIGHT_EPOCH : HL_A64_LOOP_PREFLIGHT_READY;
}

int hl_a64_trace_range_plan(const uint32_t *words, size_t count, uint64_t pc,
                            hl_a64_range_plan *output) {
    hl_a64_range_plan plan = {0};
    uint32_t definitions = 0;
    int have_base = 0;
    if (output == NULL) return 0;
    memset(output, 0, sizeof(*output));
    if (words == NULL || count == 0 || count > HL_A64_TRACE_MAX_WORDS ||
        pc > UINT64_MAX - (count - 1) * UINT64_C(4))
        return 0;

    for (size_t index = 0; index < count; index++) {
        uint64_t instruction = pc + index * 4;
        hl_a64_instruction_effect effect = hl_a64_trace_effect(words[index], instruction);
        hl_a64_memory memory;
        if (effect.terminal || effect.control) break;
        if (effect.gpr_definitions == UINT32_MAX) return 0;
        definitions |= effect.gpr_definitions;
        if (!hl_a64_memory_decode(words[index], instruction, &memory)) continue;
        if (memory.vector || memory.writeback || memory.kind == HL_A64_MEMORY_LITERAL ||
            memory.kind == HL_A64_MEMORY_REGISTER || memory.kind == HL_A64_MEMORY_PAIR ||
            memory.kind == HL_A64_MEMORY_EXCLUSIVE || memory.kind == HL_A64_MEMORY_ATOMIC ||
            memory.kind == HL_A64_MEMORY_STRUCTURE || memory.kind == HL_A64_MEMORY_PREFETCH ||
            (memory.kind != HL_A64_MEMORY_UNSIGNED && memory.kind != HL_A64_MEMORY_UNSCALED) ||
            memory.bytes == 0 || memory.bytes > INT64_MAX ||
            memory.offset > INT64_MAX - (int64_t)memory.bytes)
            return 0;
        if (!have_base) {
            plan.base = memory.base;
            plan.minimum = memory.offset;
            plan.maximum = memory.offset + (int64_t)memory.bytes;
            have_base = 1;
        } else {
            if (memory.base != plan.base) return 0;
            if (memory.offset < plan.minimum) plan.minimum = memory.offset;
            if (memory.offset + (int64_t)memory.bytes > plan.maximum)
                plan.maximum = memory.offset + (int64_t)memory.bytes;
        }
        plan.permissions |= memory.read ? HL_NATIVE_ACCESS_READ : 0;
        plan.permissions |= memory.write ? HL_NATIVE_ACCESS_WRITE : 0;
        plan.members |= UINT32_C(1) << index;
        plan.access_count++;
    }
    if (!have_base || plan.access_count < 2 ||
        (definitions & register_definition(plan.base)) != 0)
        return 0;
    *output = plan;
    return 1;
}

static int add_signed_checked(uint64_t base, int64_t offset, uint64_t *result) {
    if (offset >= 0) {
        if (base > UINT64_MAX - (uint64_t)offset) return 0;
        *result = base + (uint64_t)offset;
    } else {
        uint64_t magnitude = (uint64_t)(-(offset + 1)) + 1;
        if (base < magnitude) return 0;
        *result = base - magnitude;
    }
    return 1;
}

int hl_a64_trace_certificate_check(const hl_native_aarch64_cpu *cpu, uint64_t base,
                                   int64_t minimum, int64_t maximum, uint32_t required,
                                   uint64_t mapping_incarnation, uint64_t expected_authority,
                                   uint64_t active_authority, uint64_t *delta) {
    uint64_t first, last, token;
    if (delta != NULL) *delta = 0;
    if (cpu == NULL || delta == NULL || minimum >= maximum || required == 0 ||
        (required & ~(HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE)) != 0 ||
        mapping_incarnation == 0 || expected_authority == 0 ||
        active_authority != expected_authority ||
        !add_signed_checked(base, minimum, &first) ||
        !add_signed_checked(base, maximum, &last) || last <= first)
        return 0;
    token = __atomic_load_n(&cpu->read_token, __ATOMIC_ACQUIRE);
    if (token != mapping_incarnation || cpu->read_incarnation != token ||
        cpu->read_count == 0 || cpu->read_count > 4)
        return 0;
    for (uint64_t index = 0; index < cpu->read_count; index++) {
        const uint64_t *view = cpu->read_views[index];
        if (view[1] <= view[0] || first < view[0] || last > view[1] ||
            (view[3] & required) != required)
            continue;
        *delta = view[2];
        return 1;
    }
    return 0;
}

static int trace_build(const hl_a64_source *source, uint64_t pc, size_t count, void *buffer,
                       size_t capacity, hl_a64_trace_result *output, void *ibtc,
                       const hl_native_direct_authority *authority, uint64_t expected_authority,
                       hl_a64_trace_density *density_output) {
    hl_a64_fetch_result fetched;
    hl_a64_assembler assembler;
    hl_a64_guard guards[HL_A64_SOURCE_MAX_WORDS] = {0};
    hl_a64_density_family guard_families[HL_A64_SOURCE_MAX_WORDS] = {0};
    hl_a64_trace_density density = {0};
    hl_a64_budget_guard budget_guard;
    size_t guard_count = 0;
    if (output == NULL || expected_authority == 0) return 0;
    memset(output, 0, sizeof(*output));
    if (buffer == NULL || capacity < HL_A64_STUB_MAX_BYTES || count == 0 ||
        count > HL_A64_TRACE_MAX_WORDS || !hl_a64_source_fetch(source, pc, count, &fetched) ||
        !hl_a64_assembler_begin(&assembler, buffer, buffer, capacity))
        return 0;
    hl_a64_stub_prologue(&assembler);
    output->body_offset = hl_a64_assembler_size(&assembler);
    /* Every entry path, including direct chains and IBTC hits, publishes a
     * position-independent pointer into the exact executing cache entry.  Bit
     * zero distinguishes it from the aligned indirect-patch site carried in
     * the same transient CPU field. */
    hl_a64_emit32(&assembler, 0x10000011u); /* adr x17,. */
    hl_a64_addi(&assembler, 17, 17, 1);
    hl_a64_str(&assembler, 17, 28, (int)offsetof(hl_native_aarch64_cpu, indirect_site));
    hl_a64_stub_budget_begin(&assembler, pc, &budget_guard);
    output->source_first = pc;
    for (size_t index = 0; index < count; index++) {
        uint64_t instruction = pc + index * 4;
        size_t begin = hl_a64_assembler_size(&assembler);
        hl_a64_memory_sites memory_sites = {0};
        if (fetched.words[index] == 0xD4000001u) {
            output->source_last = instruction + 4;
            output->terminal = HL_NATIVE_EXIT_SYSCALL;
            hl_a64_stub_exit(&assembler, HL_NATIVE_EXIT_SYSCALL, instruction);
            if (!append_unknown(output, begin, hl_a64_assembler_size(&assembler), instruction)) return 0;
            density_record(&density, HL_A64_DENSITY_CONTROL,
                           (hl_a64_assembler_size(&assembler) - begin) / sizeof(uint32_t), 1);
            break;
        }
        uint32_t *direct_patch = NULL;
        uint32_t *taken_patch = NULL;
        uint64_t direct_target = 0;
        uint64_t taken_target = 0;
        if (hl_a64_direct_chain(&assembler, fetched.words[index], instruction,
                                &direct_patch, &direct_target) ||
            hl_a64_indirect_body(&assembler, fetched.words[index], instruction, ibtc) ||
            hl_a64_conditional_chain(&assembler, fetched.words[index], instruction,
                                     &direct_patch, &direct_target, &taken_patch, &taken_target)) {
            output->source_last = instruction + 4;
            output->terminal = HL_NATIVE_EXIT_BRANCH;
            if (!append_unknown(output, begin, hl_a64_assembler_size(&assembler), instruction)) return 0;
            density_record(&density, HL_A64_DENSITY_CONTROL,
                           (hl_a64_assembler_size(&assembler) - begin) / sizeof(uint32_t), 1);
            if (direct_patch != NULL) {
                output->relocations[0] = (hl_native_relocation){
                    .code_offset = (uint64_t)((uint8_t *)direct_patch - (uint8_t *)buffer),
                    .target_guest = direct_target,
                    .expected = *direct_patch,
                };
                output->relocation_count = 1;
            }
            if (taken_patch != NULL) {
                output->relocations[1] = (hl_native_relocation){
                    .code_offset = (uint64_t)((uint8_t *)taken_patch - (uint8_t *)buffer),
                    .target_guest = taken_target,
                    .expected = *taken_patch,
                };
                output->relocation_count = 2;
            }
            break;
        }
        if (!body(&assembler, fetched.words[index], instruction) &&
            !hl_a64_single_direct_body(&assembler, fetched.words[index], instruction, authority,
                                       &guards[guard_count], &memory_sites) &&
            !hl_a64_zero_body(&assembler, fetched.words[index], instruction, &guards[guard_count], &memory_sites) &&
            !hl_a64_ordered_body(&assembler, fetched.words[index], instruction, &guards[guard_count], &memory_sites) &&
            !hl_a64_simd_immediate_body(&assembler, fetched.words[index]) &&
            !hl_a64_single_body(&assembler, fetched.words[index], instruction, &guards[guard_count], &memory_sites) &&
            !hl_a64_pair_body(&assembler, fetched.words[index], instruction, &guards[guard_count], &memory_sites) &&
            !hl_a64_structure_body(&assembler, fetched.words[index], instruction, &guards[guard_count], &memory_sites)) {
            if (index == 0) {
                memset(buffer, 0, capacity);
                memset(output, 0, sizeof(*output));
                return 0;
            }
            output->source_last = instruction + 4;
            output->terminal = HL_NATIVE_EXIT_FALLBACK;
            hl_a64_stub_exit(&assembler, HL_NATIVE_EXIT_FALLBACK, instruction);
            if (!append_unknown(output, begin, hl_a64_assembler_size(&assembler), instruction)) return 0;
            density_record(&density, HL_A64_DENSITY_OTHER,
                           (hl_a64_assembler_size(&assembler) - begin) / sizeof(uint32_t), 1);
            break;
        }
        if (!hl_a64_assembler_ok(&assembler)) {
            memset(buffer, 0, capacity);
            memset(output, 0, sizeof(*output));
            return 0;
        }
        hl_a64_density_family family = density_family(fetched.words[index], instruction, &memory_sites);
        density_record(&density, family,
                       (hl_a64_assembler_size(&assembler) - begin) / sizeof(uint32_t), 1);
        if (guards[guard_count].below != NULL) {
            guard_families[guard_count] = family;
            guard_count++;
        }
        if (memory_sites.count != 0) {
            if (!append_memory(output, &memory_sites, instruction)) return 0;
        } else if (!append_unknown(output, begin, hl_a64_assembler_size(&assembler), instruction)) {
            return 0;
        }
    }
    if (output->terminal == 0) {
        output->source_last = pc + count * 4;
        output->terminal = HL_NATIVE_EXIT_BRANCH;
        hl_a64_stub_exit(&assembler, HL_NATIVE_EXIT_BRANCH, output->source_last);
    }
    output->instruction_count = (output->source_last - output->source_first) / 4;
    hl_a64_stub_budget_finish(&assembler, &budget_guard, (uint32_t)output->instruction_count);
    if (output->provenance_count + guard_count > HL_NATIVE_PROVENANCE_MAX) {
        memset(buffer, 0, capacity);
        memset(output, 0, sizeof(*output));
        return 0;
    }
    for (size_t index = 0; index < guard_count; index++) {
        size_t begin = hl_a64_assembler_size(&assembler);
        hl_a64_guard_finish(&assembler, &guards[index]);
        if (!append_unknown(output, begin, hl_a64_assembler_size(&assembler), guards[index].pc)) return 0;
        density_add(&density.families[guard_families[index]].cold_words,
                    (hl_a64_assembler_size(&assembler) - begin) / sizeof(uint32_t), &density);
    }
    if (!hl_a64_assembler_ok(&assembler)) {
        memset(buffer, 0, capacity);
        memset(output, 0, sizeof(*output));
        return 0;
    }
    output->code_size = hl_a64_assembler_size(&assembler);
    density.total_words = output->code_size / sizeof(uint32_t);
    uint64_t attributed = 0;
    for (size_t family = 0; family < HL_A64_DENSITY_FAMILY_COUNT; family++) {
        density_add(&attributed, density.families[family].hot_words, &density);
        density_add(&attributed, density.families[family].cold_words, &density);
    }
    density.overhead_words = attributed <= density.total_words ? density.total_words - attributed : 0;
    if (density_output != NULL) *density_output = density;
    return 1;
}

int hl_a64_trace_build(const hl_a64_source *source, uint64_t pc, size_t count, void *buffer,
                       size_t capacity, hl_a64_trace_result *output) {
    return trace_build(source, pc, count, buffer, capacity, output, NULL, NULL,
                       source == NULL ? 0 : source->mapping_incarnation, NULL);
}

int hl_a64_trace_build_density(const hl_a64_source *source, uint64_t pc, size_t count,
                               void *buffer, size_t capacity, hl_a64_trace_result *output,
                               hl_a64_trace_density *density) {
    if (density == NULL) return 0;
    memset(density, 0, sizeof(*density));
    return trace_build(source, pc, count, buffer, capacity, output, NULL, NULL,
                       source == NULL ? 0 : source->mapping_incarnation, density);
}

int hl_a64_trace_build_direct(const hl_a64_source *source, uint64_t pc, size_t count, void *buffer,
                              size_t capacity, const hl_native_direct_authority *authority,
                              uint64_t expected_authority,
                              hl_a64_trace_result *output) {
    return trace_build(source, pc, count, buffer, capacity, output, NULL, authority,
                       expected_authority, NULL);
}

hl_native_status hl_a64_trace_cache_direct(hl_native_executor *executor, const hl_a64_source *source, uint64_t pc,
                                           size_t count, void *buffer, size_t capacity,
                                           const hl_native_direct_authority *authority,
                                           uint64_t expected_authority,
                                           hl_native_code *code, uint32_t *hit) {
    hl_a64_trace_result trace;
    hl_native_translation_key key;
    hl_native_emission emission;
    if (executor == NULL || source == NULL || code == NULL || hit == NULL || count == 0 ||
        count > HL_A64_TRACE_MAX_WORDS || pc > UINT64_MAX - count * 4 || expected_authority == 0 ||
        executor->authority_generation != expected_authority)
        return HL_NATIVE_ARGUMENT;
    key = (hl_native_translation_key){
        .guest = pc,
        .mapping_incarnation = source->mapping_incarnation,
        .instruction_epoch = source->instruction_epoch,
        .source_first = pc,
        .source_last = pc + count * 4,
        .memory_mode = executor->memory_mode,
        .authority_generation = expected_authority,
    };
    if (hl_native_translation_lookup(executor, &key, code) == HL_NATIVE_HIT) {
        if (code->source_last <= key.source_last) {
            *hit = 1;
            return HL_NATIVE_OK;
        }
        hl_native_status gate = hl_native_executor_gate_enter(executor);
        if (gate != HL_NATIVE_OK) return gate;
        int writing = 0;
        gate = hl_native_cache_write_begin(executor->cache);
        if (gate == HL_NATIVE_OK) writing = 1;
        if (gate == HL_NATIVE_OK)
            gate = hl_native_cache_relocations_invalidate(executor->cache, pc, pc + 4);
        if (gate == HL_NATIVE_OK)
            gate = hl_native_cache_invalidate(executor->cache, pc, pc + 4, NULL);
        if (gate == HL_NATIVE_OK)
            memset(executor->ibtc, 0, HL_NATIVE_IBTC_COUNT * sizeof(*executor->ibtc));
        if (writing) {
            hl_native_status end = hl_native_cache_write_end(executor->cache);
            if (gate == HL_NATIVE_OK) gate = end;
        }
        hl_native_executor_gate_leave(executor);
        if (gate != HL_NATIVE_OK) return gate;
    }
    *hit = 0;
    if (!trace_build(source, pc, count, buffer, capacity, &trace, executor->ibtc, authority,
                     expected_authority, NULL))
        return HL_NATIVE_STATE;
    key.source_last = trace.source_last;
    if (trace.provenance_count == 0) return HL_NATIVE_STATE;
    emission = (hl_native_emission){.bytes = buffer, .size = trace.code_size,
                                    .body_offset = trace.body_offset, .provenance = trace.provenance,
                                    .provenance_count = trace.provenance_count,
                                    .relocations = trace.relocations,
                                    .relocation_count = trace.relocation_count};
    for (uint32_t index = 0; index < trace.relocation_count; index++) {
        trace.relocations[index].target_instruction_epoch = 0;
        trace.relocations[index].target_epoch_known = 0;
    }
    hl_native_status status = hl_native_translation_publish(executor, &key, &emission);
    if (status != HL_NATIVE_OK) return status;
    return hl_native_translation_lookup(executor, &key, code) == HL_NATIVE_HIT ? HL_NATIVE_OK : HL_NATIVE_STATE;
}

hl_native_status hl_a64_trace_cache(hl_native_executor *executor, const hl_a64_source *source, uint64_t pc,
                                    size_t count, void *buffer, size_t capacity, hl_native_code *code,
                                    uint32_t *hit) {
    return hl_a64_trace_cache_direct(executor, source, pc, count, buffer, capacity, NULL,
                                     executor == NULL ? 0 : executor->authority_generation, code, hit);
}
