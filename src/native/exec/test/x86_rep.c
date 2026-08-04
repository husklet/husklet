#include "support.h"
#include "../src/arch/x86_64/projection.h"
#include "../src/executor.h"
#include "../src/translation.h"

#include <stdio.h>
#include <sys/mman.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "x86_rep:%d: %s\n", __LINE__, #value); return 1; } } while (0)

#if defined(__aarch64__)
static hl_native_status executable_begin(void *opaque) {
    test_memory *memory = opaque;
    memory->begin_calls++;
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_WRITE) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static hl_native_status executable_end(void *opaque) {
    test_memory *memory = opaque;
    memory->end_calls++;
    __builtin___clear_cache((char *)memory->writable, (char *)memory->writable + memory->capacity);
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_EXEC) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static uint64_t subtract_flags(uint64_t left, uint64_t right, size_t width, uint64_t preserved) {
    uint64_t mask = width == 8 ? UINT64_MAX : (UINT64_C(1) << (width * 8)) - 1;
    uint64_t sign = UINT64_C(1) << (width * 8 - 1);
    uint64_t result = (left - right) & mask;
    uint64_t flags = preserved & ~UINT64_C(0x8d5);
    left &= mask;
    right &= mask;
    if (left < right) flags |= UINT64_C(1);
    if (__builtin_parity((unsigned)(uint8_t)result) == 0) flags |= UINT64_C(4);
    if (((left ^ right ^ result) & UINT64_C(0x10)) != 0) flags |= UINT64_C(0x10);
    if (result == 0) flags |= UINT64_C(0x40);
    if ((result & sign) != 0) flags |= UINT64_C(0x80);
    if (((left ^ right) & (left ^ result) & sign) != 0) flags |= UINT64_C(0x800);
    return flags;
}

typedef struct compare_fault {
    hl_native_projection_view view;
    uint64_t fault;
    uint32_t result;
    size_t calls;
} compare_fault;

static uint32_t resolve_compare_fault(void *opaque, uint64_t address, uint64_t size,
                                      uint32_t access, uint64_t mapping_incarnation,
                                      uint64_t instruction_epoch,
                                      hl_native_projection_view *view) {
    compare_fault *fault = opaque;
    (void)instruction_epoch;
    fault->calls++;
    fault->fault = address;
    if (fault->result == HL_NATIVE_OPERAND_RESOLVED) {
        if (address > fault->view.guest_last ||
            size > fault->view.guest_last - address ||
            access != HL_NATIVE_ACCESS_READ ||
            mapping_incarnation != fault->view.mapping_incarnation)
            return HL_NATIVE_OPERAND_DECLINED;
        *view = fault->view;
    }
    return fault->result;
}

static void compare_code(uint8_t *code, size_t *length, size_t width, int scan,
                         uint8_t repeat, int address_32, uint8_t segment) {
    *length = 0;
    if (repeat != 0) code[(*length)++] = repeat;
    if (address_32) code[(*length)++] = 0x67;
    if (segment != 0) code[(*length)++] = segment;
    if (width == 2) code[(*length)++] = 0x66;
    if (width == 8) code[(*length)++] = 0x48;
    code[(*length)++] = (uint8_t)((scan ? 0xae : 0xa6) + (width != 1));
}

static int compare_contract(hl_native_executor *executor) {
    static const uint8_t prefixes[][2] = {{0, 0}, {0xf2, 1}, {0xf3, 1}};
    static const size_t widths[] = {1, 2, 4, 8};
    uint64_t left[4] = {UINT64_C(0x80), UINT64_C(0x7fff), UINT64_C(0x80000000),
                        UINT64_C(0x7fffffffffffffff)};
    uint64_t right[4] = {UINT64_C(0x01), UINT64_C(0xffff), UINT64_C(0x7fffffff),
                         UINT64_C(0xffffffffffffffff)};
    uint8_t source_bytes[128] = {0};
    uint8_t destination_bytes[128] = {0};
    hl_native_projection_view views[] = {
        {0x12000, 0x12080, (uint64_t)(uintptr_t)source_bytes, 9, HL_NATIVE_ACCESS_READ, 0},
        {0x13000, 0x13080, (uint64_t)(uintptr_t)destination_bytes, 9, HL_NATIVE_ACCESS_READ, 0},
    };
    hl_native_projection projection = {views, 2, 9, 1};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    for (size_t operation = 0; operation < 2; ++operation) {
        for (size_t width_index = 0; width_index < 4; ++width_index) {
            size_t width = widths[width_index];
            for (size_t prefix_index = 0; prefix_index < 3; ++prefix_index) {
                uint8_t code[4] = {0};
                size_t length = 0;
                if (prefixes[prefix_index][1]) code[length++] = prefixes[prefix_index][0];
                if (width == 2) code[length++] = 0x66;
                if (width == 8) code[length++] = 0x48;
                code[length++] = (uint8_t)((operation == 0 ? 0xa6 : 0xae) + (width != 1));
                uint64_t pc = UINT64_C(0x11000) + operation * 0x100 + width_index * 0x10 + prefix_index;
                hl_native_source_span instruction = {pc, code, length, 9, 17};
                hl_native_source source = {&instruction, 1, 9, 17};
                hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
                    .architecture = HL_NATIVE_X86_64, .mapping_epoch = 9, .budget = 4,
                    .source = &source, .projection = &projection};
                memset(source_bytes, 0, sizeof(source_bytes));
                memset(destination_bytes, 0, sizeof(destination_bytes));
                for (size_t index = 0; index < 3; ++index) {
                    memcpy(source_bytes + index * width, &left[width_index], width);
                    memcpy(destination_bytes + index * width, &left[width_index], width);
                }
                memcpy(destination_bytes + width, &right[width_index], width);
                uint64_t initial_flags = UINT64_C(0x202) | UINT64_C(0x8d5);
                hl_native_x86_64_cpu state = {.program = pc, .flags = initial_flags,
                    .registers = {[0] = width == 8 ? left[width_index] :
                                      UINT64_C(0xa5a5a5a500000000) | left[width_index],
                                  [1] = prefix_index == 0 ? 99 : 3,
                                  [6] = 0x12000, [7] = 0x13000}, .dirty_first = UINT64_MAX};
                hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
                    .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
                CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
                size_t completed = prefix_index == 0 ? 1 : prefix_index == 1 ? 1 : 2;
                uint64_t flag_left = operation == 0 ? left[width_index] :
                    (state.registers[0] & (width == 8 ? UINT64_MAX : (UINT64_C(1) << (8 * width)) - 1));
                uint64_t flag_right = prefix_index == 2 ? right[width_index] : left[width_index];
                if (state.program != pc + length || state.executed != completed)
                    fprintf(stderr, "compare case op=%zu width=%zu prefix=%zu pc=%llx executed=%llu\n",
                            operation, width, prefix_index, (unsigned long long)state.program,
                            (unsigned long long)state.executed);
                CHECK(state.program == pc + length && state.executed == completed);
                CHECK(state.registers[7] == UINT64_C(0x13000) + completed * width);
                CHECK(operation != 0 || state.registers[6] == UINT64_C(0x12000) + completed * width);
                CHECK(state.registers[1] == (prefix_index == 0 ? 99 : 3 - completed));
                CHECK(state.flags == subtract_flags(flag_left, flag_right, width, initial_flags));
            }
        }
    }
    {
        static const uint8_t zero[] = {0xf3, 0xa6};
        hl_native_source_span instruction = {0x11500, zero, sizeof(zero), 9, 17};
        hl_native_source source = {&instruction, 1, 9, 17};
        hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
            .architecture = HL_NATIVE_X86_64, .mapping_epoch = 9, .budget = 1,
            .source = &source, .projection = &projection};
        hl_native_x86_64_cpu state = {.program = 0x11500, .flags = 0xad7,
            .registers = {[1] = 0, [6] = 0x12000, [7] = 0x13000}, .dirty_first = UINT64_MAX};
        hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
            .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(state.program == 0x11502 && state.executed == 1 && state.flags == 0xad7 &&
              state.registers[6] == 0x12000 && state.registers[7] == 0x13000);
    }
    /* DF and every REP termination position: middle, last, and exhaustion. */
    for (size_t operation = 0; operation < 2; ++operation) {
        for (size_t reverse = 0; reverse < 2; ++reverse) {
            for (size_t stop = 0; stop < 3; ++stop) {
                uint8_t code[6];
                size_t length;
                compare_code(code, &length, 1, operation != 0, 0xf2, 0, 0);
                uint64_t pc = UINT64_C(0x11600) + operation * 0x100 + reverse * 0x10 + stop;
                hl_native_source_span instruction = {pc, code, length, 9, 17};
                hl_native_source source = {&instruction, 1, 9, 17};
                hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
                    .architecture = HL_NATIVE_X86_64, .mapping_epoch = 9, .budget = 4,
                    .source = &source, .projection = &projection};
                memset(source_bytes, 0x11, 4);
                memset(destination_bytes, 0x22, 4);
                size_t equal_index = stop == 0 ? 1 : stop == 1 ? (reverse ? 0 : 3) : 4;
                if (equal_index < 4) {
                    if (operation == 0) source_bytes[equal_index] = 0x22;
                    else destination_bytes[equal_index] = 0x33;
                }
                uint64_t first = reverse ? 3 : 0;
                uint64_t completed = stop == 0 ? (reverse ? 3 : 2) : 4;
                hl_native_x86_64_cpu state = {.program = pc,
                    .flags = reverse ? UINT64_C(1) << 10 : 0,
                    .registers = {[0] = 0x33, [1] = 4,
                                  [6] = 0x12000 + first, [7] = 0x13000 + first},
                    .dirty_first = UINT64_MAX};
                hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
                    .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
                CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
                int64_t delta = reverse ? -(int64_t)completed : (int64_t)completed;
                CHECK(state.program == pc + length && state.registers[1] == 4 - completed);
                CHECK(state.registers[7] == UINT64_C(0x13000) + first + delta);
                CHECK(operation != 0 || state.registers[6] == UINT64_C(0x12000) + first + delta);
            }
        }
    }
    /* Address-size override zero-extends ECX/ESI/EDI; FS/GS apply only to the
     * CMPS source operand and are accepted with every operand width. */
    for (size_t segment_index = 0; segment_index < 3; ++segment_index) {
        uint8_t segment = segment_index == 0 ? 0 : segment_index == 1 ? 0x64 : 0x65;
        for (size_t width_index = 0; width_index < 4; ++width_index) {
            uint8_t code[6];
            size_t length;
            size_t width = widths[width_index];
            compare_code(code, &length, width, 0, 0xf3, 1, segment);
            uint64_t pc = UINT64_C(0x11800) + segment_index * 0x100 + width_index * 0x10;
            hl_native_source_span instruction = {pc, code, length, 9, 17};
            hl_native_source source = {&instruction, 1, 9, 17};
            hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
                .architecture = HL_NATIVE_X86_64, .mapping_epoch = 9, .budget = 2,
                .source = &source, .projection = &projection};
            uint64_t base = segment_index == 0 ? 0 : 0x40;
            memset(source_bytes, 0, sizeof(source_bytes));
            memset(destination_bytes, 0, sizeof(destination_bytes));
            memcpy(source_bytes + base, &left[width_index], width);
            memcpy(destination_bytes, &left[width_index], width);
            hl_native_x86_64_cpu state = {.program = pc,
                .fs = segment == 0x64 ? base : 0, .gs = segment == 0x65 ? base : 0,
                .registers = {[1] = UINT64_C(0xaaaaaaaa00000001),
                              [6] = UINT64_C(0xbbbbbbbb00012000),
                              [7] = UINT64_C(0xcccccccc00013000)},
                .dirty_first = UINT64_MAX};
            hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
                .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
            CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
            CHECK(state.program == pc + length && state.registers[1] == 0);
            CHECK(state.registers[6] == UINT64_C(0x12000) + width);
            CHECK(state.registers[7] == UINT64_C(0x13000) + width);
        }
    }
    /* A split authenticated span completes through repeated bulk entries. */
    {
        uint8_t code[6];
        size_t length;
        compare_code(code, &length, 1, 0, 0xf3, 0, 0);
        hl_native_projection_view split[] = {
            {0x12000, 0x12002, (uint64_t)(uintptr_t)source_bytes, 9, HL_NATIVE_ACCESS_READ, 0},
            {0x12002, 0x12004, (uint64_t)(uintptr_t)(source_bytes + 2), 9, HL_NATIVE_ACCESS_READ, 0},
            {0x13000, 0x13002, (uint64_t)(uintptr_t)destination_bytes, 9, HL_NATIVE_ACCESS_READ, 0},
            {0x13002, 0x13004, (uint64_t)(uintptr_t)(destination_bytes + 2), 9, HL_NATIVE_ACCESS_READ, 0},
        };
        hl_native_projection split_projection = {split, 4, 9, 3};
        hl_native_source_span instruction = {0x11c00, code, length, 9, 17};
        hl_native_source source = {&instruction, 1, 9, 17};
        hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
            .architecture = HL_NATIVE_X86_64, .mapping_epoch = 9, .budget = 4,
            .source = &source, .projection = &split_projection};
        memset(source_bytes, 0x5a, 4);
        memset(destination_bytes, 0x5a, 4);
        hl_native_x86_64_cpu state = {.program = 0x11c00,
            .registers = {[1] = 4, [6] = 0x12000, [7] = 0x13000}, .dirty_first = UINT64_MAX};
        hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
            .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(state.program == UINT64_C(0x11c00) + length && state.registers[1] == 0 &&
              state.registers[6] == 0x12004 && state.registers[7] == 0x13004);
    }
    /* Missing second spans take the scalar precise-fault path. The completed
     * first element stays committed, then a new run resumes after resolution. */
    {
        uint8_t code[6];
        size_t length;
        compare_code(code, &length, 1, 0, 0xf3, 0, 0);
        hl_native_projection_view initial[] = {
            {0x12000, 0x12001, (uint64_t)(uintptr_t)source_bytes, 9, HL_NATIVE_ACCESS_READ, 0},
            {0x13000, 0x13004, (uint64_t)(uintptr_t)destination_bytes, 9, HL_NATIVE_ACCESS_READ, 0},
        };
        hl_native_projection initial_projection = {initial, 2, 9, 1};
        hl_native_source_span instruction = {0x11d00, code, length, 9, 17};
        hl_native_source source = {&instruction, 1, 9, 17};
        compare_fault fault = {.view = {0x12001, 0x12004,
            (uint64_t)(uintptr_t)(source_bytes + 1), 9, HL_NATIVE_ACCESS_READ, 0},
            .result = HL_NATIVE_OPERAND_FAULT};
        hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
            .architecture = HL_NATIVE_X86_64, .mapping_epoch = 9, .budget = 4,
            .source = &source, .projection = &initial_projection,
            .operand_resolve = resolve_compare_fault, .operand_context = &fault};
        memset(source_bytes, 0x6b, 4);
        memset(destination_bytes, 0x6b, 4);
        hl_native_x86_64_cpu state = {.program = 0x11d00,
            .registers = {[1] = 4, [6] = 0x12000, [7] = 0x13000}, .dirty_first = UINT64_MAX};
        hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
            .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_FAULT && fault.calls == 1 && fault.fault == 0x12001);
        CHECK(state.program == 0x11d00 && state.registers[1] == 3 &&
              state.registers[6] == 0x12001 && state.registers[7] == 0x13001);
        fault.result = HL_NATIVE_OPERAND_RESOLVED;
        fault.calls = 0;
        request.budget = 3;
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(fault.calls == 1 && state.program == UINT64_C(0x11d00) + length &&
              state.registers[1] == 0 && state.registers[6] == 0x12004 &&
              state.registers[7] == 0x13004);
    }
    return 0;
}

static int rep_contract(void) {
    static const uint8_t rep_movsb[] = {0xf3, 0xa4};
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    memory.write_begin = executable_begin;
    memory.write_end = executable_end;
    hl_native_config config = test_config(&memory, 0);
    hl_native_executor *executor = NULL;
    uint8_t source_bytes[16] = "abcdefghijklmno";
    uint8_t destination_bytes[16] = {0};
    hl_native_source_span instruction = {0xd000, rep_movsb, sizeof(rep_movsb), 7, 16};
    hl_native_source source = {&instruction, 1, 7, 16};
    hl_native_projection_view views[] = {
        {0xe000, 0xe010, (uint64_t)(uintptr_t)source_bytes, 7, HL_NATIVE_ACCESS_READ, 0},
        {0xf000, 0xf010, (uint64_t)(uintptr_t)destination_bytes, 7,
         HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE, 0},
    };
    hl_native_projection projection = {views, 2, 7, 1};
    hl_native_x86_64_cpu state = {.program = 0xd000,
        .registers = {[1] = 8, [6] = 0xe000, [7] = 0xf000}, .dirty_first = UINT64_MAX};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7, .budget = 3,
        .source = &source, .projection = &projection};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};

    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.program == 0xd000 &&
          state.registers[1] == 5 && state.registers[6] == 0xe003 &&
          state.registers[7] == 0xf003 && state.executed == 3 &&
          memcmp(destination_bytes, "abc", 3) == 0);

    memset(destination_bytes, 0, sizeof(destination_bytes));
    request.budget = 8;
    state = (hl_native_x86_64_cpu){.program = 0xd000,
        .registers = {[1] = 8, [6] = 0xe000, [7] = 0xf000},
        .memory_first = 0xf000, .memory_last = 0xf010,
        .memory_permissions = HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE,
        .dirty_first = 0xf00f, .dirty_last = 0xf010, .dirty_count = HL_X86_DIRTY_CAPACITY};
    for (size_t index = 0; index < HL_X86_DIRTY_CAPACITY; ++index) {
        state.dirty_records[index][0] = 0x1000 + index;
        state.dirty_records[index][1] = 0x2000 + index;
        state.dirty_records[index][2] = 0x3000 + index;
        state.dirty_records[index][3] = 0x4000 + index;
    }
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_EPOCH && state.program == 0xd000 &&
          state.registers[1] == 8 && state.registers[6] == 0xe000 &&
              state.registers[7] == 0xf000 && state.dirty_count == HL_X86_DIRTY_CAPACITY &&
          state.dirty_overflow == 0);
    CHECK(memcmp(destination_bytes, "\0\0\0\0\0\0\0\0", 8) == 0);
    for (size_t index = 0; index < HL_X86_DIRTY_CAPACITY; ++index)
        CHECK(state.dirty_records[index][0] == 0x1000 + index &&
              state.dirty_records[index][3] == 0x4000 + index);

    {
        static const struct {
            uint8_t code[4];
            uint8_t length;
            uint8_t width;
        } forms[] = {
            {{0xf3, 0xa4}, 2, 1}, {{0xf3, 0x66, 0xa5}, 3, 2},
            {{0xf3, 0xa5}, 2, 4}, {{0xf3, 0x48, 0xa5}, 3, 8},
            {{0xf3, 0xaa}, 2, 1}, {{0xf3, 0x66, 0xab}, 3, 2},
            {{0xf3, 0xab}, 2, 4}, {{0xf3, 0x48, 0xab}, 3, 8},
        };
        for (size_t index = 0; index < sizeof(forms) / sizeof(forms[0]); ++index) {
            uint64_t pc = 0xd100 + index * 0x10;
            instruction = (hl_native_source_span){pc, forms[index].code, forms[index].length, 7, 16};
            memset(destination_bytes, 0, sizeof(destination_bytes));
            request.source = &source;
            request.budget = 2;
            state = (hl_native_x86_64_cpu){.program = pc,
                .registers = {[0] = UINT64_C(0x6867666564636261), [1] = 2,
                              [6] = 0xe000, [7] = 0xf000}, .dirty_first = UINT64_MAX};
            CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
            CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.program == pc + forms[index].length &&
                  state.registers[1] == 0 &&
                  state.registers[7] == UINT64_C(0xf000) + 2u * forms[index].width);
            if (index < 4) {
                CHECK(memcmp(destination_bytes, source_bytes, 2 * forms[index].width) == 0);
            } else {
                CHECK(memcmp(destination_bytes, "abcdefgh", forms[index].width) == 0 &&
                      memcmp(destination_bytes + forms[index].width, "abcdefgh", forms[index].width) == 0);
            }
        }
    }
    {
        uint8_t overlap[16] = "abcdefghijklmno";
        views[0] = (hl_native_projection_view){0xe000, 0xe010, (uint64_t)(uintptr_t)overlap, 7,
                                               HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE, 0};
        projection = (hl_native_projection){views, 1, 7, 0};
        request.projection = &projection;
        instruction = (hl_native_source_span){0xd200, rep_movsb, sizeof(rep_movsb), 7, 16};
        state = (hl_native_x86_64_cpu){.program = 0xd200,
            .registers = {[1] = 6, [6] = 0xe000, [7] = 0xe002}, .dirty_first = UINT64_MAX};
        request.budget = 6;
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(memcmp(overlap, "abababab", 8) == 0);

        memcpy(overlap, "abcdefghijklmno", 16);
        instruction.guest_first = 0xd210;
        state = (hl_native_x86_64_cpu){.program = 0xd210,
            .registers = {[1] = 6, [6] = 0xe002, [7] = 0xe000}, .dirty_first = UINT64_MAX};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(memcmp(overlap, "cdefghgh", 8) == 0);

        memcpy(overlap, "abcdefghijklmno", 16);
        instruction.guest_first = 0xd220;
        state = (hl_native_x86_64_cpu){.program = 0xd220, .flags = UINT64_C(1) << 10,
            .registers = {[1] = 6, [6] = 0xe007, [7] = 0xe005}, .dirty_first = UINT64_MAX};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(memcmp(overlap, "ghghghgh", 8) == 0);
    }
    {
        views[0] = (hl_native_projection_view){0xe000, 0xe010, (uint64_t)(uintptr_t)source_bytes, 7,
                                               HL_NATIVE_ACCESS_READ, 0};
        views[1] = (hl_native_projection_view){0xf000, 0xf010, (uint64_t)(uintptr_t)destination_bytes, 7,
                                               HL_NATIVE_ACCESS_READ, 0};
        projection = (hl_native_projection){views, 2, 7, 1};
        request.projection = &projection;
        instruction = (hl_native_source_span){0xd300, rep_movsb, sizeof(rep_movsb), 7, 16};
        memset(destination_bytes, 0, sizeof(destination_bytes));
        state = (hl_native_x86_64_cpu){.program = 0xd300,
            .registers = {[1] = 4, [6] = 0xe000, [7] = 0xf000}, .dirty_first = UINT64_MAX};
        request.budget = 4;
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK && state.program == 0xd300 &&
              state.registers[1] == 4 && memcmp(destination_bytes, "\0\0\0\0", 4) == 0);

        views[1].permissions = 7;
        instruction.guest_first = 0xd310;
        state = (hl_native_x86_64_cpu){.program = 0xd310,
            .registers = {[1] = 4, [6] = 0xe000, [7] = 0xf000}, .dirty_first = UINT64_MAX};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_EPOCH && state.program == 0xd312 &&
              state.registers[1] == 0 && state.dirty_first == 0xf000 && state.dirty_last == 0xf004 &&
              memcmp(destination_bytes, source_bytes, 4) == 0);
    }
    {
        hl_native_projection_view boundary_views[] = {
            {0xe000, 0xe010, (uint64_t)(uintptr_t)source_bytes, 7, HL_NATIVE_ACCESS_READ, 0},
            {0xf000, 0xf004, (uint64_t)(uintptr_t)destination_bytes, 7,
             HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE, 0},
            {0xf004, 0xf010, (uint64_t)(uintptr_t)(destination_bytes + 4), 7,
             HL_NATIVE_ACCESS_READ, 0},
        };
        projection = (hl_native_projection){boundary_views, 3, 7, 1};
        request.projection = &projection;
        instruction = (hl_native_source_span){0xd380, rep_movsb, sizeof(rep_movsb), 7, 16};
        memset(destination_bytes, 0, sizeof(destination_bytes));
        state = (hl_native_x86_64_cpu){.program = 0xd380,
            .registers = {[1] = 8, [6] = 0xe000, [7] = 0xf000}, .dirty_first = UINT64_MAX};
        request.budget = 8;
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK && state.program == 0xd380 &&
              state.registers[1] == 4 && state.registers[6] == 0xe004 &&
              state.registers[7] == 0xf004 && memcmp(destination_bytes, "abcd\0\0\0\0", 8) == 0);
    }
    {
        static const uint8_t address_rep[] = {0xf3, 0x67, 0xa4};
        static const uint8_t segment_rep[] = {0xf3, 0x64, 0xa4};
        const uint8_t *codes[] = {address_rep, segment_rep};
        views[0] = (hl_native_projection_view){0xe000, 0xe010, (uint64_t)(uintptr_t)source_bytes, 7,
                                               HL_NATIVE_ACCESS_READ, 0};
        views[1] = (hl_native_projection_view){0xf000, 0xf010, (uint64_t)(uintptr_t)destination_bytes, 7,
                                               HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE, 0};
        projection = (hl_native_projection){views, 2, 7, 1};
        request.projection = &projection;
        request.budget = 1;
        for (size_t index = 0; index < 2; ++index) {
            uint64_t pc = 0xd400 + index * 0x10;
            instruction = (hl_native_source_span){pc, codes[index], 3, 7, 16};
            state = (hl_native_x86_64_cpu){.program = pc,
                .registers = {[1] = 1, [6] = 0xe000, [7] = 0xf000}, .dirty_first = UINT64_MAX};
            CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
            CHECK(state.program == pc + 3 && state.registers[1] == 0 &&
                  state.registers[6] == 0xe001 && state.registers[7] == 0xf001);
        }

        static const uint8_t mixed_rep[] = {0xf3, 0x66, 0x48, 0xa5};
        static const uint8_t repeated_rex_rep[] = {0xf3, 0x48, 0x48, 0xa5};
        const uint8_t *noncanonical[] = {mixed_rep, repeated_rex_rep};
        for (size_t index = 0; index < 2; ++index) {
            uint64_t pc = 0xd480 + index * 0x10;
            instruction = (hl_native_source_span){pc, noncanonical[index], 4, 7, 16};
            memset(destination_bytes, 0, sizeof(destination_bytes));
            state = (hl_native_x86_64_cpu){.program = pc,
                .registers = {[1] = 1, [6] = 0xe000, [7] = 0xf000}, .dirty_first = UINT64_MAX};
            CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
            hl_native_translation_key key = {pc, 7, 16, pc, pc + 4, 0, 0};
            hl_native_code translated;
            if (index == 0) {
                CHECK(hl_native_translation_lookup(executor, &key, &translated) == HL_NATIVE_MISS &&
                      state.program == pc + 4 && memcmp(destination_bytes, source_bytes, 8) == 0);
            } else {
                CHECK(hl_native_translation_lookup(executor, &key, &translated) == HL_NATIVE_MISS &&
                      output.kind == HL_NATIVE_EXIT_FALLBACK && state.program == pc &&
                      state.registers[1] == 1 && memcmp(destination_bytes, "\0\0\0\0\0\0\0\0", 8) == 0);
            }
        }

        instruction = (hl_native_source_span){0xd500, rep_movsb, sizeof(rep_movsb), 7, 16};
        state = (hl_native_x86_64_cpu){.program = 0xd500, .dirty_first = UINT64_MAX};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.program == 0xd502 &&
              state.registers[1] == 0 && state.executed == 1 && state.budget == 0);
    }
    CHECK(compare_contract(executor) == 0);
    hl_native_destroy(executor);
    return 0;
}
#else
static int rep_contract(void) { return 0; }
#endif

int main(void) { return rep_contract(); }
