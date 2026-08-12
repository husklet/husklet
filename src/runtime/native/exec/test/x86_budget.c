#include "support.h"
#include "../src/executor.h"

#include <stdio.h>
#include <sys/mman.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "x86_budget:%d: %s\n", __LINE__, #value); return 1; } } while (0)

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

static int budget_contract(void) {
    static const uint8_t five[] = {0x90, 0x90, 0x90, 0x90, 0xeb, 0x0a};
    static const uint8_t twelve[] = {
        0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0xeb, 0xe3,
    };
    static const uint8_t loop[] = {0x90, 0x90, 0x90, 0x90, 0x83, 0xf8, 0x00, 0x74, 0xf7};
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    memory.write_begin = executable_begin;
    memory.write_end = executable_end;
    hl_native_config config = test_config(&memory, 0);
    hl_native_executor *executor = NULL;
    hl_native_x86_64_cpu state = {0};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};

    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    {
        hl_native_source_span spans[] = {
            {0x7100, five, sizeof(five), 7, 16},
            {0x7110, twelve, sizeof(twelve), 7, 16},
        };
        hl_native_source source = {spans, 2, 7, 16};
        request.source = &source;
        request.budget = 12;
        state = (hl_native_x86_64_cpu){.program = 0x7110};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 12 && state.budget == 0);
        request.budget = 5;
        state = (hl_native_x86_64_cpu){.program = 0x7100};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 5 && state.budget == 0);
        request.budget = 16;
        state = (hl_native_x86_64_cpu){.program = 0x7100};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.program == 0x7110 &&
              state.executed == 5 && state.budget == 11);
    }
    {
        hl_native_source_span spans[] = {
            {0x7200, five, sizeof(five), 7, 16},
            {0x7210, loop, sizeof(loop), 7, 16},
        };
        hl_native_source source = {spans, 2, 7, 16};
        request.source = &source;
        request.budget = 6;
        state = (hl_native_x86_64_cpu){.program = 0x7210};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 6 && state.budget == 0);
        request.budget = 5;
        state = (hl_native_x86_64_cpu){.program = 0x7200};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 5 && state.budget == 0);
        request.budget = 16;
        state = (hl_native_x86_64_cpu){.program = 0x7200};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.program == 0x7210 &&
              state.executed == 11 && state.budget == 5);
    }
    {
        static const uint8_t pcmp[] = {0x66, 0x0f, 0x3a, 0x63, 0xc1, 0x08};
        hl_native_source_span spans[] = {{0x7300, pcmp, sizeof(pcmp), 7, 16}};
        hl_native_source source = {spans, 1, 7, 16};
        const uint64_t comparison = UINT64_C(1) | UINT64_C(1) << 2 | UINT64_C(1) << 4 |
                                    UINT64_C(1) << 6 | UINT64_C(1) << 7 | UINT64_C(1) << 11;
        request.source = &source;
        request.budget = 0;
        state = (hl_native_x86_64_cpu){.program = 0x7300, .flags = UINT64_C(0xad7)};
        state.vectors[0] = UINT64_C(0x61);
        state.vectors[2] = UINT64_C(0x62);
        state.registers[1] = UINT64_C(0xfeedface);
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 0);
        CHECK(state.registers[1] == UINT64_C(0xfeedface) && state.flags == UINT64_C(0xad7));
        request.budget = 1;
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 1 && state.budget == 0);
        CHECK(state.registers[1] == 1);
        CHECK(state.flags == ((UINT64_C(0xad7) & ~comparison) |
                              UINT64_C(1) | UINT64_C(1) << 6 | UINT64_C(1) << 7));
    }
    {
        static const uint8_t nan_loop[] = {
            0x0f, 0x59, 0xc0,       /* mulps xmm0,xmm0 */
            0x48, 0xff, 0xc9,       /* dec rcx */
            0x75, 0xf8,             /* jne nan_loop */
        };
        hl_native_source_span span = {0x7400, nan_loop, sizeof(nan_loop), 7, 16};
        hl_native_source source = {&span, 1, 7, 16};

        request.source = &source;
        request.budget = 8;
        state = (hl_native_x86_64_cpu){.program = 0x7400, .registers = {[1] = 2}};
        state.vectors[0] = UINT64_C(0x7fc000007fc00000);
        state.vectors[1] = UINT64_C(0x7fc000007fc00000);
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK && state.program == 0x7400 &&
              state.executed == 0 && state.registers[1] == 2);
    }
    hl_native_destroy(executor);
    return 0;
}
#endif

int main(void) {
#if defined(__aarch64__)
    return budget_contract();
#else
    return 0;
#endif
}
