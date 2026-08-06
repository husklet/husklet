#include "support.h"
#include "../src/arch/x86_64/entry.h"
#include "../src/translation.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <pthread.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "run:%d: %s\n", __LINE__, #value); return 1; } } while (0)

#if defined(__aarch64__)
typedef struct resolver_gate {
    pthread_mutex_t mutex;
    pthread_cond_t changed;
    int entered;
    int released;
} resolver_gate;

static void resolver_wait(resolver_gate *gate) {
    pthread_mutex_lock(&gate->mutex);
    gate->entered = 1;
    pthread_cond_broadcast(&gate->changed);
    while (!gate->released) pthread_cond_wait(&gate->changed, &gate->mutex);
    pthread_mutex_unlock(&gate->mutex);
}

static void resolver_entered(resolver_gate *gate) {
    pthread_mutex_lock(&gate->mutex);
    while (!gate->entered) pthread_cond_wait(&gate->changed, &gate->mutex);
    pthread_mutex_unlock(&gate->mutex);
}

static void resolver_release(resolver_gate *gate) {
    pthread_mutex_lock(&gate->mutex);
    gate->released = 1;
    pthread_cond_broadcast(&gate->changed);
    pthread_mutex_unlock(&gate->mutex);
}

typedef struct run_worker {
    hl_native_executor *executor;
    hl_native_cpu *cpu;
    hl_native_run_request *request;
    hl_native_exit *output;
    hl_native_status status;
} run_worker;

static void *run_on_thread(void *opaque) {
    run_worker *worker = opaque;
    worker->status = hl_native_run(worker->executor, worker->cpu, worker->request, worker->output);
    return NULL;
}

extern int hl_native_test_spill(hl_native_aarch64_cpu *);
#endif

static int run_contract(void) {
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    hl_native_config config = test_config(&memory, 0);
    hl_native_executor *executor = NULL;
    hl_native_aarch64_cpu state = {0};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
                         .architecture = HL_NATIVE_AARCH64, .state.aarch64 = &state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
                                     .architecture = HL_NATIVE_AARCH64, .mapping_epoch = 19, .budget = 8};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    state.program = 0x12345678;
#if defined(__aarch64__)
    CHECK(hl_native_test_spill(&state) == 0);
    CHECK(state.program == 0x12345678);
#endif
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    for (unsigned iteration = 0; iteration != 64; iteration++) {
        state.code_arena_lower = UINT64_MAX;
        state.code_arena_upper = UINT64_MAX;
        state.entry_certificate_identity = UINT64_MAX;
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(state.code_arena_lower == 0 && state.code_arena_upper == 0 &&
              state.entry_certificate_identity == 0);
        CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK && output.instruction == state.program);
        CHECK(output.next == state.program && output.code == 0);
    }
    request.budget = 0;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && output.instruction == state.program);
    request.budget = 8;
    state.code_arena_lower = 1;
    state.code_arena_upper = 2;
    state.entry_certificate_identity = 3;
    state.interrupt = 7;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_INTERRUPT && output.code == 0);
    CHECK(state.code_arena_lower == 0 && state.code_arena_upper == 0 &&
          state.entry_certificate_identity == 0);
    request.memory_mode = 1;
    state.code_arena_lower = 1;
    state.code_arena_upper = 2;
    state.entry_certificate_identity = 3;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_ARGUMENT);
    CHECK(state.code_arena_lower == 0 && state.code_arena_upper == 0 &&
          state.entry_certificate_identity == 0);
    hl_native_destroy(executor);
    CHECK(host.release_calls == 1);
    return 0;
}

static int validation(void) {
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
                                     .architecture = HL_NATIVE_X86_64, .budget = 1};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu), .architecture = HL_NATIVE_AARCH64};
    CHECK(hl_native_run(NULL, &cpu, &request, &output) == HL_NATIVE_ARGUMENT);
    return 0;
}

#if defined(__aarch64__)
static hl_native_status executable_begin(void *opaque) {
    test_memory *memory = opaque;
    memory->begin_calls++;
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_WRITE) == 0
               ? HL_NATIVE_OK
               : HL_NATIVE_PLATFORM;
}

static hl_native_status executable_end(void *opaque) {
    test_memory *memory = opaque;
    memory->end_calls++;
    __builtin___clear_cache((char *)memory->writable,
                            (char *)memory->writable + memory->capacity);
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_EXEC) == 0
               ? HL_NATIVE_OK
               : HL_NATIVE_PLATFORM;
}

static int resolve_source(void *opaque, uint64_t pc, uint64_t mapping, uint64_t epoch,
                          hl_native_source_span *output) {
    const hl_native_source_span *span = opaque;
    if (pc < span->guest_first || pc >= span->guest_first + span->size ||
        mapping != span->mapping_incarnation || epoch != span->instruction_epoch)
        return 0;
    *output = *span;
    return 1;
}

typedef struct blocked_source {
    hl_native_source_span span;
    resolver_gate *gate;
} blocked_source;

static int resolve_blocked_source(void *opaque, uint64_t pc, uint64_t mapping, uint64_t epoch,
                                  hl_native_source_span *output) {
    blocked_source *source = opaque;
    resolver_wait(source->gate);
    return resolve_source(&source->span, pc, mapping, epoch, output);
}

typedef struct operand_provider {
    uint64_t guest;
    uint64_t value;
    uint32_t result;
    uint32_t calls;
    resolver_gate *gate;
} operand_provider;

static uint32_t resolve_operand(void *opaque, uint64_t address, uint64_t size, uint32_t access,
                                uint64_t mapping, uint64_t epoch, hl_native_projection_view *output) {
    operand_provider *provider = opaque;
    if (provider->gate != NULL) resolver_wait(provider->gate);
    provider->calls++;
    if (mapping != 7 || epoch != 14) return HL_NATIVE_OPERAND_EPOCH;
    if (provider->result != HL_NATIVE_OPERAND_RESOLVED) return provider->result;
    if (address != provider->guest || size != 8 || access != HL_NATIVE_ACCESS_WRITE)
        return HL_NATIVE_OPERAND_FAULT;
    *output = (hl_native_projection_view){.guest_first = address, .guest_last = address + size,
        .host_first = (uint64_t)(uintptr_t)&provider->value, .mapping_incarnation = mapping,
        .permissions = HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE};
    return HL_NATIVE_OPERAND_RESOLVED;
}

static int x86_native(void) {
    static const uint8_t program[] = {0xb8, 1, 0, 0, 0, 0x01, 0xc0, 0x0f, 0x05};
    static const uint8_t branch[] = {0xeb, 2, 0xcc, 0xcc, 0x0f, 0x05};
    static const uint8_t unsupported[] = {0x48, 0x8b, 0x03};
    static const uint8_t supported_prefix[] = {0xb8, 1, 0, 0, 0, 0xcc};
    static const uint8_t conditional_loop[] = {
        0x83, 0xe8, 0x01,       /* sub eax,1 */
        0x75, 0xfb,             /* jne 0 */
        0x0f, 0x05,             /* syscall */
    };
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    memory.write_begin = executable_begin;
    memory.write_end = executable_end;
    hl_native_config config = test_config(&memory, 0);
    hl_native_executor *executor = NULL;
    hl_native_x86_64_cpu state = {0};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
                         .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
    hl_native_source_span span = {0x1000, program, sizeof(program), 7, 11};
    hl_native_source source = {&span, 1, 7, 11};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
                                     .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7,
                                     .budget = 3, .source = &source};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    hl_native_diagnostics before = {.abi = HL_NATIVE_ABI, .size = sizeof(before)};
    hl_native_diagnostics after = {.abi = HL_NATIVE_ABI, .size = sizeof(after)};
    hl_native_diagnostics warm = {.abi = HL_NATIVE_ABI, .size = sizeof(warm)};
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);

    state.program = 0x1000;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && output.instruction == 0x1007 && output.next == 0x1009);
    CHECK(state.registers[0] == 2 && state.program == 0x1009);
    CHECK(state.budget == 0 && state.executed == 3);
    CHECK(hl_native_diagnose(executor, &before) == HL_NATIVE_OK);
    state = (hl_native_x86_64_cpu){.program = 0x1000};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(state.registers[0] == 2 && output.kind == HL_NATIVE_EXIT_SYSCALL);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    CHECK(after.cache_hits >= before.cache_hits + 1 && after.publications == before.publications);
    request.size = offsetof(hl_native_run_request, memory_mode);
    state = (hl_native_x86_64_cpu){.program = 0x1000};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(state.registers[0] == 2 && output.kind == HL_NATIVE_EXIT_SYSCALL);
    request.size = sizeof(request);

    {
        const hl_native_translation_key key = {0x1000, 7, 11, 0x1000, 0x1009, 0, 0, 0, 0, 0};
        hl_native_code code;
        CHECK(hl_native_translation_lookup(executor, &key, &code) == HL_NATIVE_HIT);
        CHECK((uint8_t *)code.body == (uint8_t *)code.entry + 2u * sizeof(uint32_t));
        state = (hl_native_x86_64_cpu){.program = 0x1000, .budget = 5, .interrupt = 1};
        hl_native_x86_64_enter(&state, code.entry);
        CHECK(state.scratch[0] == 0 && state.registers[0] == 0 && state.program == 0x1000);
        state.interrupt = 0;
        state.budget = 2;
        hl_native_x86_64_enter(&state, code.entry);
        CHECK(state.scratch[0] == 0 && state.registers[0] == 0 && state.program == 0x1000);
    }

    state = (hl_native_x86_64_cpu){.program = 0x1000};
    request.budget = 1;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && output.instruction == 0x1000);
    CHECK(state.registers[0] == 0 && state.program == 0x1000);
    CHECK(state.budget == 1 && state.executed == 0);
    state = (hl_native_x86_64_cpu){.program = 0x1000, .interrupt = 1};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_INTERRUPT && state.program == 0x1000 && state.registers[0] == 0);
    CHECK(state.budget == 1 && state.executed == 0);

    span = (hl_native_source_span){0x1800, conditional_loop, sizeof(conditional_loop), 7, 15};
    source = (hl_native_source){&span, 1, 7, 15};
    request = (hl_native_run_request){.abi = HL_NATIVE_ABI, .size = sizeof(request),
                                      .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7,
                                      .budget = 0, .source = &source};
    state = (hl_native_x86_64_cpu){.program = 0x1800, .registers = {2}};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.registers[0] == 2 && state.executed == 0);
    request.budget = 1;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.registers[0] == 2 && state.executed == 0);
    request.budget = 2;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.program == 0x1800 && state.registers[0] == 1);
    CHECK(state.executed == 2 && state.budget == 0 && state.loop_remaining == 0 && state.loop_completed == 0 &&
          state.loop_block_count == 0 && state.loop_pc == 0);
    state = (hl_native_x86_64_cpu){.program = 0x1800, .registers = {2}};
    request.budget = 4;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.program == 0x1805 && state.registers[0] == 0);
    CHECK(state.executed == 4 && state.budget == 0);
    state = (hl_native_x86_64_cpu){.program = 0x1800, .registers = {2}};
    request.budget = 5;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && output.instruction == 0x1805 && state.registers[0] == 0);
    CHECK(state.executed == 5 && state.budget == 0);
    state = (hl_native_x86_64_cpu){.program = 0x1800, .registers = {1}};
    request.budget = 3;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && state.registers[0] == 0 && state.executed == 3);

    span = (hl_native_source_span){0x2000, branch, sizeof(branch), 7, 12};
    source = (hl_native_source){&span, 1, 7, 12};
    request = (hl_native_run_request){.abi = HL_NATIVE_ABI, .size = sizeof(request),
                                      .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7,
                                      .budget = 2, .source = &source};
    state = (hl_native_x86_64_cpu){.program = 0x2000};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && output.instruction == 0x2004 && output.next == 0x2006);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    state = (hl_native_x86_64_cpu){.program = 0x2000};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && output.instruction == 0x2004);
    CHECK(hl_native_diagnose(executor, &warm) == HL_NATIVE_OK);
    CHECK(warm.publications == after.publications && warm.cache_hits > after.cache_hits);

    span = (hl_native_source_span){0x3000, unsupported, sizeof(unsupported), 7, 12};
    source = (hl_native_source){&span, 1, 7, 12};
    request.source = &source;
    state = (hl_native_x86_64_cpu){.program = 0x3000, .registers = {9}};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK && state.program == 0x3000 && state.registers[0] == 9);
    CHECK(state.budget == 2 && state.executed == 0);

    span = (hl_native_source_span){0x3100, supported_prefix, sizeof(supported_prefix), 7, 12};
    source = (hl_native_source){&span, 1, 7, 12};
    request.source = &source;
    state = (hl_native_x86_64_cpu){.program = 0x3100};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK && state.program == 0x3105 && state.registers[0] == 1);
    CHECK(state.budget == 1 && state.executed == 1);

    resolver_gate source_gate = {PTHREAD_MUTEX_INITIALIZER, PTHREAD_COND_INITIALIZER, 0, 0};
    blocked_source resolved = {{0x5000, program + 7, 2, 7, 13}, &source_gate};
    span = (hl_native_source_span){0x4000, unsupported, sizeof(unsupported), 7, 13};
    source = (hl_native_source){&span, 1, 7, 13};
    request = (hl_native_run_request){.abi = HL_NATIVE_ABI, .size = sizeof(request),
                                      .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7,
                                      .budget = 1, .source = &source, .source_context = &resolved,
                                      .source_resolve = resolve_blocked_source};
    state = (hl_native_x86_64_cpu){.program = 0x5000, .indirect_site = 0x4444};
    run_worker source_worker = {executor, &cpu, &request, &output, HL_NATIVE_STATE};
    pthread_t source_thread;
    CHECK(pthread_create(&source_thread, NULL, run_on_thread, &source_worker) == 0);
    resolver_entered(&source_gate);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_STATE);
    CHECK(hl_native_executor_gate_enter(executor) == HL_NATIVE_OK);
    hl_native_executor_gate_leave(executor);
    resolver_release(&source_gate);
    CHECK(pthread_join(source_thread, NULL) == 0 && source_worker.status == HL_NATIVE_OK);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_OK);
    CHECK(hl_native_after_fork(executor, 1) == HL_NATIVE_OK);
    CHECK(pthread_cond_destroy(&source_gate.changed) == 0);
    CHECK(pthread_mutex_destroy(&source_gate.mutex) == 0);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && output.instruction == 0x5000 && output.next == 0x5002);

    static const uint8_t operand_program[] = {
        0x48, 0x83, 0xc0, 0x01, /* add rax,1 */
        0x48, 0x89, 0x03,       /* mov [rbx],rax */
        0x0f, 0x05,             /* syscall */
    };
    resolver_gate operand_gate = {PTHREAD_MUTEX_INITIALIZER, PTHREAD_COND_INITIALIZER, 0, 0};
    operand_provider operand = {.guest = 0x7000, .result = HL_NATIVE_OPERAND_RESOLVED,
                                .gate = &operand_gate};
    span = (hl_native_source_span){0x6000, operand_program, sizeof(operand_program), 7, 14};
    source = (hl_native_source){&span, 1, 7, 14};
    request = (hl_native_run_request){.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7, .budget = 3, .source = &source,
        .operand_context = &operand, .operand_resolve = resolve_operand};
    state = (hl_native_x86_64_cpu){.program = 0x6000, .registers = {[3] = operand.guest}};
    run_worker operand_worker = {executor, &cpu, &request, &output, HL_NATIVE_STATE};
    pthread_t operand_thread;
    CHECK(pthread_create(&operand_thread, NULL, run_on_thread, &operand_worker) == 0);
    resolver_entered(&operand_gate);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_STATE);
    resolver_release(&operand_gate);
    CHECK(pthread_join(operand_thread, NULL) == 0 && operand_worker.status == HL_NATIVE_OK);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_OK);
    CHECK(hl_native_after_fork(executor, 1) == HL_NATIVE_OK);
    operand.gate = NULL;
    CHECK(pthread_cond_destroy(&operand_gate.changed) == 0);
    CHECK(pthread_mutex_destroy(&operand_gate.mutex) == 0);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && output.instruction == 0x6007);
    CHECK(state.registers[0] == 1 && operand.value == 1 && operand.calls == 1);
    CHECK(state.executed == 3 && state.budget == 0);

    operand.value = 0;
    operand.result = HL_NATIVE_OPERAND_FAULT;
    state = (hl_native_x86_64_cpu){.program = 0x6000, .registers = {[3] = operand.guest}};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_FAULT && output.instruction == 0x6004 && output.address == operand.guest);
    CHECK(state.registers[0] == 1 && operand.value == 0 && state.executed == 1 && state.budget == 2);

    operand.result = HL_NATIVE_OPERAND_EPOCH;
    state = (hl_native_x86_64_cpu){.program = 0x6000, .registers = {[3] = operand.guest}};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_EPOCH && state.registers[0] == 1 && operand.value == 0);

    operand.result = HL_NATIVE_OPERAND_DECLINED;
    state = (hl_native_x86_64_cpu){.program = 0x6000, .registers = {[3] = operand.guest}};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK && output.instruction == 0x6004 && state.executed == 1);
    uint32_t calls = operand.calls;
    span = (hl_native_source_span){0x6100, supported_prefix, sizeof(supported_prefix), 7, 14};
    source = (hl_native_source){&span, 1, 7, 14};
    request.source = &source;
    state.program = 0x6100;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK && output.instruction == 0x6105 && operand.calls == calls);

    hl_native_destroy(executor);
    executor = NULL;
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    request.source_resolve = NULL;
    request.source_context = NULL;
    state = (hl_native_x86_64_cpu){.program = 0x5000, .indirect_site = 0x4444};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK && output.access == HL_NATIVE_ACCESS_EXECUTE);
    CHECK(output.instruction == 0x4444 && output.next == 0x5000 && output.address == 0x5000);
    hl_native_destroy(executor);
    return 0;
}
#else
static int x86_native(void) { return 0; }
#endif

int main(void) { return run_contract() || validation() || x86_native(); }
