#include "support.h"
#include "../src/arch/x86_64/projection.h"
#include "../src/executor.h"
#include "../src/translation.h"

#include <stdio.h>
#include <sys/mman.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "x86_rep:%d: %s\n", __LINE__, #value); return 1; } } while (0)

#if defined(__aarch64__)
typedef struct quantum_observation {
    uint64_t calls;
    uint64_t executed;
    uint64_t admitted;
} quantum_observation;

static uint32_t admit_quantum(void *opaque, uint64_t executed, uint64_t admitted) {
    quantum_observation *observation = opaque;
    observation->calls++;
    observation->executed = executed;
    observation->admitted = admitted;
    return 1;
}

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
        {0xe000, 0xe010, (uint64_t)(uintptr_t)source_bytes, 7, HL_NATIVE_ACCESS_READ,
         HL_NATIVE_WRITE_EXACT, 0},
        {0xf000, 0xf010, (uint64_t)(uintptr_t)destination_bytes, 7,
          HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE, HL_NATIVE_WRITE_EXACT, 1},
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
        .memory_first = 0xe000, .memory_last = 0xe010,
        .memory_permissions = HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE,
        .dirty_view_first = 0xe000, .dirty_view_last = 0xe010,
        .dirty_first = 0xe00f, .dirty_last = 0xe010,
        .dirty_count = HL_X86_DIRTY_CAPACITY};
    for (size_t index = 0; index < HL_X86_DIRTY_CAPACITY; ++index) {
        state.dirty_records[index][0] = 0x1000 + index;
        state.dirty_records[index][1] = 0x2000 + index;
        state.dirty_records[index][2] = 0x3000 + index;
        state.dirty_records[index][3] = 0x4000 + index;
    }
    state.dirty_records[0][0] = 0xe000;
    state.dirty_records[0][1] = 0xe010;
    state.dirty_records[0][2] = 0xe00e;
    state.dirty_records[0][3] = 0xe010;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.program == 0xd002 &&
          state.registers[1] == 0 && state.registers[6] == 0xe008 &&
          state.registers[7] == 0xf008 && state.dirty_count == HL_X86_DIRTY_CAPACITY &&
          state.dirty_overflow == 0 && memcmp(destination_bytes, "abcdefgh", 8) == 0);
    CHECK(state.dirty_records[0][0] == 0xe000 && state.dirty_records[0][1] == 0xe010 &&
          state.dirty_records[0][2] == 0xe00e && state.dirty_records[0][3] == 0xe010);

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
        uint8_t unused[4][16] = {{0}};
        hl_native_projection_view many_views[] = {
            {0x1000, 0x1010, (uint64_t)(uintptr_t)source_bytes, 7,
             HL_NATIVE_ACCESS_READ, HL_NATIVE_WRITE_EXACT, 0},
            {0x2000, 0x2010, (uint64_t)(uintptr_t)destination_bytes, 7,
             HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE, HL_NATIVE_WRITE_EXACT, 1},
            {0x3000, 0x3010, (uint64_t)(uintptr_t)unused[0], 7,
             HL_NATIVE_ACCESS_READ, HL_NATIVE_WRITE_EXACT, 2},
            {0x4000, 0x4010, (uint64_t)(uintptr_t)unused[1], 7,
             HL_NATIVE_ACCESS_READ, HL_NATIVE_WRITE_EXACT, 3},
            {0x5000, 0x5010, (uint64_t)(uintptr_t)unused[2], 7,
             HL_NATIVE_ACCESS_READ, HL_NATIVE_WRITE_EXACT, 4},
            {0x6000, 0x6010, (uint64_t)(uintptr_t)unused[3], 7,
             HL_NATIVE_ACCESS_READ, HL_NATIVE_WRITE_EXACT, 5},
        };
        projection = (hl_native_projection){many_views, 6, 7, 2};
        request.projection = &projection;
        instruction = (hl_native_source_span){0xd080, rep_movsb, sizeof(rep_movsb), 7, 16};
        memset(destination_bytes, 0, sizeof(destination_bytes));
        state = (hl_native_x86_64_cpu){.program = 0xd080,
            .registers = {[1] = 8, [6] = 0x1000, [7] = 0x2000}, .dirty_first = UINT64_MAX};
        request.budget = 8;
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.program == 0xd082 &&
              state.registers[1] == 0 && state.registers[6] == 0x1008 &&
              state.registers[7] == 0x2008 && state.executed == 8 &&
              state.dirty_first == 0x2000 && state.dirty_last == 0x2008 &&
              memcmp(destination_bytes, source_bytes, 8) == 0);
    }

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
        projection = (hl_native_projection){views, 2, 7, 1};
        request.projection = &projection;
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
                                               HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE,
                                               HL_NATIVE_WRITE_EXACT, 0};
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
                                               HL_NATIVE_ACCESS_READ, HL_NATIVE_WRITE_EXACT, 0};
        views[1] = (hl_native_projection_view){0xf000, 0xf010, (uint64_t)(uintptr_t)destination_bytes, 7,
                                                HL_NATIVE_ACCESS_READ, HL_NATIVE_WRITE_EXACT, 1};
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
            {0xe000, 0xe010, (uint64_t)(uintptr_t)source_bytes, 7, HL_NATIVE_ACCESS_READ,
             HL_NATIVE_WRITE_EXACT, 0},
            {0xf000, 0xf004, (uint64_t)(uintptr_t)destination_bytes, 7,
              HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE, HL_NATIVE_WRITE_EXACT, 1},
             {0xf004, 0xf010, (uint64_t)(uintptr_t)(destination_bytes + 4), 7,
              HL_NATIVE_ACCESS_READ, HL_NATIVE_WRITE_EXACT, 2},
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
        static const uint8_t rep_then_syscall[] = {0xf3, 0xa4, 0x0f, 0x05};
        quantum_observation observation = {0};
        views[0] = (hl_native_projection_view){0xe000, 0xe010,
                                               (uint64_t)(uintptr_t)source_bytes, 7,
                                               HL_NATIVE_ACCESS_READ, HL_NATIVE_WRITE_EXACT, 0};
        views[1] = (hl_native_projection_view){0xf000, 0xf010,
                                               (uint64_t)(uintptr_t)destination_bytes, 7,
                                               HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE,
                                               HL_NATIVE_WRITE_EXACT, 1};
        projection = (hl_native_projection){views, 2, 7, 1};
        request.projection = &projection;
        instruction = (hl_native_source_span){0xd580, rep_then_syscall,
                                               sizeof(rep_then_syscall), 7, 16};
        memset(destination_bytes, 0, sizeof(destination_bytes));
        state = (hl_native_x86_64_cpu){.program = 0xd580,
            .registers = {[1] = 2, [6] = 0xe000, [7] = 0xf000},
            .dirty_first = UINT64_MAX};
        request.budget = 2;
        request.quantum_context = &observation;
        request.quantum_poll = admit_quantum;
        request.quantum_grant = 2;
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && output.instruction == 0xd582 &&
              state.program == 0xd584 && state.executed == 3 && state.budget == 1 &&
              observation.calls == 1 && observation.executed == 2 &&
              observation.admitted == 2 && memcmp(destination_bytes, "ab", 2) == 0);
        request.quantum_context = NULL;
        request.quantum_poll = NULL;
        request.quantum_grant = 0;
    }
    {
        static const uint8_t address_rep[] = {0xf3, 0x67, 0xa4};
        static const uint8_t segment_rep[] = {0xf3, 0x64, 0xa4};
        const uint8_t *codes[] = {address_rep, segment_rep};
        views[0] = (hl_native_projection_view){0xe000, 0xe010, (uint64_t)(uintptr_t)source_bytes, 7,
                                               HL_NATIVE_ACCESS_READ, HL_NATIVE_WRITE_EXACT, 0};
        views[1] = (hl_native_projection_view){0xf000, 0xf010, (uint64_t)(uintptr_t)destination_bytes, 7,
                                               HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE,
                                                HL_NATIVE_WRITE_EXACT, 1};
        projection = (hl_native_projection){views, 2, 7, 1};
        request.projection = &projection;
        request.budget = 1;
        for (size_t index = 0; index < 2; ++index) {
            uint64_t pc = 0xd400 + index * 0x10;
            instruction = (hl_native_source_span){pc, codes[index], 3, 7, 16};
            state = (hl_native_x86_64_cpu){.program = pc,
                .registers = {[1] = 1, [6] = 0xe000, [7] = 0xf000}, .dirty_first = UINT64_MAX};
            CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
            hl_native_translation_key key = {pc, 7, 16, pc, pc + 3, 0, 0};
            hl_native_code translated;
            CHECK(hl_native_translation_lookup(executor, &key, &translated) == HL_NATIVE_HIT);
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
                CHECK(hl_native_translation_lookup(executor, &key, &translated) == HL_NATIVE_HIT &&
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
    hl_native_destroy(executor);
    return 0;
}
#else
static int rep_contract(void) { return 0; }
#endif

int main(void) { return rep_contract(); }
