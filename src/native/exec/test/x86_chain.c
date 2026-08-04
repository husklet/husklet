#include "support.h"
#include "../src/arch/x86_64/entry.h"
#include "../src/arch/x86_64/flags.h"
#include "../src/executor.h"
#include "../src/translation.h"

#include <stdio.h>
#include <sys/mman.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "x86_chain:%d: %s\n", __LINE__, #value); return 1; } } while (0)

#if defined(__aarch64__)
enum { X86_DISPATCH_RETURN_WORDS = 12 };

static uint32_t *direct_exit_site(const hl_native_code *code) {
    return (uint32_t *)((uint8_t *)code->entry + code->code_size) -
           (X86_DISPATCH_RETURN_WORDS + 1);
}

static uint32_t *conditional_exit_site(const hl_native_code *code, unsigned edge) {
    return (uint32_t *)((uint8_t *)code->entry + code->code_size) -
           (X86_DISPATCH_RETURN_WORDS + 2u - edge);
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

static int chain_contract(void) {
    static const uint8_t to_second[] = {0xeb, 0x0e};
    static const uint8_t to_first[] = {0xeb, 0xee};
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    memory.write_begin = executable_begin;
    memory.write_end = executable_end;
    hl_native_config config = test_config(&memory, 0);
    hl_native_executor *executor = NULL;
    hl_native_x86_64_cpu state = {.program = 0x7000};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
    hl_native_source_span spans[] = {
        {0x7000, to_second, sizeof(to_second), 7, 16},
        {0x7010, to_first, sizeof(to_first), 7, 16},
    };
    hl_native_source source = {spans, 2, 7, 16};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7, .budget = 1, .source = &source};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    hl_native_translation_key first_key = {0x7000, 7, 16, 0x7000, 0x7002, 0, 0};
    hl_native_translation_key second_key = {0x7010, 7, 16, 0x7010, 0x7012, 0, 0};
    hl_native_code first, second;
    hl_native_change invalidate = {.abi = HL_NATIVE_ABI, .size = sizeof(invalidate),
        .kind = HL_NATIVE_INVALIDATE, .mapping_epoch = 7};

    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    hl_native_status run_status = hl_native_run(executor, &cpu, &request, &output);
    CHECK(run_status == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 1 && state.budget == 0);
    request.budget = 4;
    state = (hl_native_x86_64_cpu){.program = 0x7010};
    run_status = hl_native_run(executor, &cpu, &request, &output);
    CHECK(run_status == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 4 && state.budget == 0);
    CHECK(hl_native_translation_lookup(executor, &first_key, &first) == HL_NATIVE_HIT);
    CHECK(hl_native_translation_lookup(executor, &second_key, &second) == HL_NATIVE_HIT);
    uint32_t *first_tail = direct_exit_site(&first);
    uint32_t *second_tail = direct_exit_site(&second);
    CHECK((*first_tail == UINT32_C(0x14000001)) !=
          (*second_tail == UINT32_C(0x14000001)));

    state = (hl_native_x86_64_cpu){.program = 0x7000, .budget = 4, .interrupt = 1};
    hl_native_x86_64_enter(&state, first.entry);
    CHECK(state.program == 0x7000 && state.scratch[0] == 0 && state.executed == 0);

    invalidate.first = 0x7010;
    invalidate.last = 0x7012;
    CHECK(hl_native_changed(executor, &invalidate, 1) == HL_NATIVE_OK);
    CHECK(*first_tail == UINT32_C(0x14000001));
    CHECK(hl_native_translation_lookup(executor, &first_key, &first) == HL_NATIVE_HIT);
    CHECK(hl_native_translation_lookup(executor, &second_key, &second) == HL_NATIVE_MISS);

    request.budget = 1;
    state = (hl_native_x86_64_cpu){.program = 0x7010};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 1 && state.budget == 0);
    CHECK(hl_native_translation_lookup(executor, &second_key, &second) == HL_NATIVE_HIT);
    second_tail = direct_exit_site(&second);
    CHECK((*first_tail == UINT32_C(0x14000001)) !=
          (*second_tail == UINT32_C(0x14000001)));

    invalidate.first = 0x7000;
    invalidate.last = 0x7002;
    CHECK(hl_native_changed(executor, &invalidate, 1) == HL_NATIVE_OK);
    CHECK(*second_tail == UINT32_C(0x14000001));
    CHECK(hl_native_translation_lookup(executor, &first_key, &first) == HL_NATIVE_MISS);
    CHECK(hl_native_translation_lookup(executor, &second_key, &second) == HL_NATIVE_HIT);

    {
        static const uint8_t choose[] = {0x83, 0xf8, 0x00, 0x74, 0x0b};
        static const uint8_t fallthrough[] = {0xeb, 0x19};
        static const uint8_t taken[] = {0xeb, 0x0e};
        static const uint8_t syscall[] = {0x0f, 0x05};
        hl_native_source_span conditional_spans[] = {
            {0x8000, choose, sizeof(choose), 7, 16},
            {0x8005, fallthrough, sizeof(fallthrough), 7, 16},
            {0x8010, taken, sizeof(taken), 7, 16},
            {0x8020, syscall, sizeof(syscall), 7, 16},
        };
        hl_native_source conditional_source = {conditional_spans, 4, 7, 16};
        hl_native_translation_key choose_key = {0x8000, 7, 16, 0x8000, 0x8005, 0, 0};
        hl_native_code choose_code;

        request.source = &conditional_source;
        request.budget = 2;
        state = (hl_native_x86_64_cpu){.program = 0x8000, .registers = {0}};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.program == 0x8010 && state.executed == 2);
        request.budget = 1;
        state = (hl_native_x86_64_cpu){.program = 0x8020};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && state.executed == 1);
        request.budget = 2;
        state = (hl_native_x86_64_cpu){.program = 0x8010};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && state.executed == 2);
        state = (hl_native_x86_64_cpu){.program = 0x8005};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && state.executed == 2);
        CHECK(hl_native_translation_lookup(executor, &choose_key, &choose_code) == HL_NATIVE_HIT);
        uint32_t *fallthrough_tail = conditional_exit_site(&choose_code, 0);
        uint32_t *taken_tail = conditional_exit_site(&choose_code, 1);
        CHECK(*fallthrough_tail != UINT32_C(0x14000002));
        CHECK(*taken_tail != UINT32_C(0x14000001));

        request.budget = 4;
        state = (hl_native_x86_64_cpu){.program = 0x8000, .registers = {0}};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && state.executed == 4 &&
              (state.flags & HL_X86_RFLAGS_ZF) != 0);
        state = (hl_native_x86_64_cpu){.program = 0x8000, .registers = {1}};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && state.executed == 4 &&
              (state.flags & HL_X86_RFLAGS_ZF) == 0);
        state = (hl_native_x86_64_cpu){.program = 0x8000, .budget = 4, .interrupt = 1};
        hl_native_x86_64_enter(&state, choose_code.entry);
        CHECK(state.program == 0x8000 && state.scratch[0] == 0);

        invalidate.kind = HL_NATIVE_INVALIDATE;
        invalidate.mapping_epoch = 7;
        invalidate.first = 0x8005;
        invalidate.last = 0x8007;
        CHECK(hl_native_changed(executor, &invalidate, 1) == HL_NATIVE_OK);
        CHECK(*fallthrough_tail == UINT32_C(0x14000002));
        CHECK(*taken_tail != UINT32_C(0x14000001));
    }
    {
        static const uint8_t caller[] = {0xe8, 0x0b, 0, 0, 0};
        static const uint8_t continuation[] = {0x0f, 0x05};
        static const uint8_t callee[] = {0xb8, 2, 0, 0, 0, 0xc3};
        uint64_t stack = 0;
        hl_native_source_span call_spans[] = {
            {0x9000, caller, sizeof(caller), 7, 16},
            {0x9005, continuation, sizeof(continuation), 7, 16},
            {0x9010, callee, sizeof(callee), 7, 16},
        };
        hl_native_source call_source = {call_spans, 3, 7, 16};
        hl_native_projection_view stack_view = {0xa000, 0xa008, (uint64_t)(uintptr_t)&stack, 7,
            HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE, 0};
        hl_native_projection projection = {&stack_view, 1, 7};
        hl_native_translation_key caller_key = {0x9000, 7, 16, 0x9000, 0x9005, 0, 0};
        hl_native_code caller_code;
        hl_native_diagnostics before = {.abi = HL_NATIVE_ABI, .size = sizeof(before)};
        hl_native_diagnostics after = {.abi = HL_NATIVE_ABI, .size = sizeof(after)};

        request.source = &call_source;
        request.projection = &projection;
        request.budget = 1;
        state = (hl_native_x86_64_cpu){.program = 0x9000, .registers = {[4] = 0xa008}};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.program == 0x9010 &&
              state.registers[4] == 0xa000 && stack == 0x9005 && state.executed == 1);
        request.budget = 3;
        state = (hl_native_x86_64_cpu){.program = 0x9010, .registers = {[4] = 0xa000}};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && state.registers[0] == 2 && state.executed == 3);
        size_t return_slot = (0x9005u >> 2) & (HL_NATIVE_IBTC_COUNT - 1u);
        CHECK(executor->ibtc[return_slot].target == 0x9005 &&
              executor->ibtc[return_slot].body != NULL && state.indirect_site == 0);
        CHECK(hl_native_translation_lookup(executor, &caller_key, &caller_code) == HL_NATIVE_HIT);
        uint32_t *caller_tail = direct_exit_site(&caller_code);
        CHECK(*caller_tail != UINT32_C(0x14000001));

        stack = 0;
        request.budget = 4;
        state = (hl_native_x86_64_cpu){.program = 0x9000, .registers = {[4] = 0xa008}};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && state.registers[0] == 2 &&
              state.registers[4] == 0xa008 && stack == 0x9005 && state.executed == 4 &&
              state.indirect_site == 0);

        stack = 0x9005;
        request.budget = 1;
        state = (hl_native_x86_64_cpu){.program = 0x9015, .registers = {[4] = 0xa000}};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.program == 0x9005 &&
              state.registers[4] == 0xa008 && state.executed == 1 && state.indirect_site == 0);

        /* A colliding stale prediction is an identity miss. It must never be
         * entered, and normal dispatch repairs the slot with the real target. */
        executor->ibtc[return_slot].target = 0x49005;
        executor->ibtc[return_slot].body = (void *)(uintptr_t)1;
        stack = 0x9005;
        request.budget = 2;
        state = (hl_native_x86_64_cpu){.program = 0x9015, .registers = {[4] = 0xa000}};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && state.program == 0x9007 &&
              state.registers[4] == 0xa008 && state.executed == 2);
        CHECK(executor->ibtc[return_slot].target == 0x9005 &&
              executor->ibtc[return_slot].body != NULL);

        /* The return stack read faults before RSP, PC, accounting, or the
         * predictor can change architectural state. */
        stack_view.permissions = HL_NATIVE_ACCESS_WRITE;
        request.budget = 1;
        state = (hl_native_x86_64_cpu){.program = 0x9015, .registers = {[4] = 0xa000}};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK && state.program == 0x9015 &&
              state.registers[4] == 0xa000 && state.executed == 0);
        stack_view.permissions = HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE;
        state = (hl_native_x86_64_cpu){.program = 0x9000, .registers = {[4] = 0xa008},
                                       .budget = 4, .interrupt = 1};
        hl_native_x86_64_enter(&state, caller_code.entry);
        CHECK(state.program == 0x9000 && state.registers[4] == 0xa008 && stack == 0x9005);

        stack_view.permissions = HL_NATIVE_ACCESS_READ;
        request.budget = 1;
        state = (hl_native_x86_64_cpu){.program = 0x9000, .registers = {[4] = 0xa008}};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK && state.program == 0x9000 &&
              state.registers[4] == 0xa008 && state.executed == 0);
        stack_view.permissions = HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE;

        invalidate.kind = HL_NATIVE_INVALIDATE;
        invalidate.mapping_epoch = 7;
        invalidate.first = 0x9010;
        invalidate.last = 0x9016;
        CHECK(hl_native_diagnose(executor, &before) == HL_NATIVE_OK);
        CHECK(hl_native_changed(executor, &invalidate, 1) == HL_NATIVE_OK);
        CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
        CHECK(before.mapping_epoch == 7 && before.cache_generation == after.cache_generation &&
              after.live_blocks + 2 == before.live_blocks && after.invalidations == before.invalidations + 2);
        CHECK(executor->ibtc[return_slot].target == 0x9005 &&
              executor->ibtc[return_slot].body != NULL);
        CHECK(*caller_tail == UINT32_C(0x14000001));
        CHECK(hl_native_translation_lookup(executor, &caller_key, &caller_code) == HL_NATIVE_HIT);
        invalidate.first = 0x9000;
        invalidate.last = 0x9005;
        CHECK(hl_native_changed(executor, &invalidate, 1) == HL_NATIVE_OK);
        CHECK(hl_native_translation_lookup(executor, &caller_key, &caller_code) == HL_NATIVE_MISS);
    }
    {
        static const uint8_t outer[] = {0xe8, 0x0b, 0, 0, 0};
        static const uint8_t done[] = {0x0f, 0x05};
        static const uint8_t inner[] = {0xe8, 0x0b, 0, 0, 0};
        static const uint8_t unwind[] = {0xc3};
        static const uint8_t leaf[] = {0xc3};
        uint64_t stack[2] = {0};
        hl_native_source_span nested_spans[] = {
            {0xb000, outer, sizeof(outer), 7, 16},
            {0xb005, done, sizeof(done), 7, 16},
            {0xb010, inner, sizeof(inner), 7, 16},
            {0xb015, unwind, sizeof(unwind), 7, 16},
            {0xb020, leaf, sizeof(leaf), 7, 16},
        };
        hl_native_source nested_source = {nested_spans, 5, 7, 16};
        hl_native_projection_view nested_view = {0xc000, 0xc010, (uint64_t)(uintptr_t)stack, 7,
            HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE, 0};
        hl_native_projection nested_projection = {&nested_view, 1, 7};

        request.source = &nested_source;
        request.projection = &nested_projection;
        request.budget = 1;
        state = (hl_native_x86_64_cpu){.program = 0xb000, .registers = {[4] = 0xc010}};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        state = (hl_native_x86_64_cpu){.program = 0xb010, .registers = {[4] = 0xc008}};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        state = (hl_native_x86_64_cpu){.program = 0xb020, .registers = {[4] = 0xc000}};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);

        stack[0] = stack[1] = 0;
        request.budget = 2;
        state = (hl_native_x86_64_cpu){.program = 0xb000, .registers = {[4] = 0xc010}};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.program == 0xb020 && state.executed == 2 &&
              state.budget == 0 && state.registers[4] == 0xc000 &&
              stack[1] == 0xb005 && stack[0] == 0xb015);

        request.budget = 5;
        stack[0] = stack[1] = 0;
        state = (hl_native_x86_64_cpu){.program = 0xb000, .registers = {[4] = 0xc010}};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && state.executed == 5 &&
              state.registers[4] == 0xc010 && state.program == 0xb007);
        CHECK(executor->ibtc[(0xb015u >> 2) & (HL_NATIVE_IBTC_COUNT - 1u)].target == 0xb015);
        CHECK(executor->ibtc[(0xb005u >> 2) & (HL_NATIVE_IBTC_COUNT - 1u)].target == 0xb005);

        /* Repeat with both return targets hot: nested depth does not allocate
         * predictor state and therefore has no RSB underflow/overflow mode. */
        stack[0] = stack[1] = 0;
        state = (hl_native_x86_64_cpu){.program = 0xb000, .registers = {[4] = 0xc010}};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && state.executed == 5 &&
              state.registers[4] == 0xc010 && state.program == 0xb007);
        CHECK((uint32_t)atomic_load_explicit(&executor->admission, memory_order_acquire) == 0);
    }
    invalidate.kind = HL_NATIVE_REPLACE;
    invalidate.mapping_epoch = 8;
    invalidate.first = 0;
    invalidate.last = 0;
    CHECK(hl_native_changed(executor, &invalidate, 1) == HL_NATIVE_OK);
    CHECK(*second_tail == UINT32_C(0x14000001));
    CHECK(hl_native_translation_lookup(executor, &second_key, &second) == HL_NATIVE_EPOCH);
    hl_native_destroy(executor);
    CHECK(host.release_calls == 1);
    return 0;
}

static int rewritten_callee_contract(void) {
    static const uint8_t caller[] = {0xe8, 0xfb, 0, 0, 0};
    static const uint8_t continuation[] = {0x0f, 0x05};
    uint8_t callee[256] = {0};
    uint64_t stack = 0;
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    memory.write_begin = executable_begin;
    memory.write_end = executable_end;
    hl_native_config config = test_config(&memory, 0);
    hl_native_executor *executor = NULL;
    hl_native_x86_64_cpu state = {0};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
    hl_native_source_span spans[] = {
        {0xd000, caller, sizeof(caller), 7, 16},
        {0xd005, continuation, sizeof(continuation), 7, 16},
        {0xd100, callee, sizeof(callee), 7, 16},
    };
    hl_native_source source = {spans, 3, 7, 16};
    hl_native_projection_view stack_view = {0xe000, 0xe008, (uint64_t)(uintptr_t)&stack, 7,
        HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE, 0};
    hl_native_projection projection = {&stack_view, 1, 7};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7, .source = &source,
        .projection = &projection};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    hl_native_change invalidate = {.abi = HL_NATIVE_ABI, .size = sizeof(invalidate),
        .kind = HL_NATIVE_INVALIDATE, .mapping_epoch = 7, .first = 0xd100, .last = 0xd200};

    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    for (unsigned round = 0; round < 512; ++round) {
        unsigned count = 1u + round % 40u;
        uint32_t step = 1u + round % 7u;
        size_t cursor = 0;
        uint64_t input = UINT64_C(0x1000) + round * 97u;
        uint64_t expected = input;
        memset(callee, 0xcc, sizeof(callee));
        callee[cursor++] = 0x48; callee[cursor++] = 0x89; callee[cursor++] = 0xf8;
        for (unsigned index = 1; index <= count; ++index) {
            uint32_t immediate = index * step;
            callee[cursor++] = 0x48; callee[cursor++] = 0x05;
            memcpy(callee + cursor, &immediate, sizeof(immediate));
            cursor += sizeof(immediate);
            expected += immediate;
        }
        callee[cursor++] = 0xc3;
        if (round != 0) CHECK(hl_native_changed(executor, &invalidate, 1) == HL_NATIVE_OK);
        stack = 0;
        request.budget = count + 4u;
        state = (hl_native_x86_64_cpu){.program = 0xd000,
            .registers = {[4] = 0xe008, [7] = input}};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && state.registers[0] == expected &&
              state.registers[7] == input && state.registers[4] == 0xe008);
    }
    hl_native_destroy(executor);
    CHECK(host.release_calls == 1);
    return 0;
}
#else
static int chain_contract(void) { return 0; }
static int rewritten_callee_contract(void) { return 0; }
#endif

int main(void) {
    int status = chain_contract();
    return status != 0 ? status : rewritten_callee_contract();
}
