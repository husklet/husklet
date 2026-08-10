#include "../include/executor.h"
#include "../src/arch/aarch64/entry.h"
#include "../src/executor.h"
#include "../cache/private.h"
#include "../src/arch/aarch64/projection.h"
#include "../src/translation.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "a64_cycles:%d: %s\n", __LINE__, #value); return 1; } } while (0)

#if defined(__aarch64__)
void enter_register_16(void);
__asm__(".type enter_register_16,%function\n"
        "enter_register_16:\n"
        "mov x28,x0\n"
        "ldr x30,[x0,#1104]\n"
        "ldr x16,[x0,#128]\n"
        "br x16\n");
#endif

typedef struct executable_memory {
    uint8_t *address;
    uint64_t capacity;
} executable_memory;

static hl_native_status reserve(void *opaque, uint64_t capacity, uint64_t alignment,
                                uint32_t dual, hl_native_mapping *output) {
    executable_memory *memory = opaque;
    (void)alignment;
    if (dual != 0) return HL_NATIVE_PLATFORM;
    void *address = mmap(NULL, (size_t)capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (address == MAP_FAILED) return HL_NATIVE_MEMORY;
    memory->address = address;
    memory->capacity = capacity;
    *output = (hl_native_mapping){.abi = HL_NATIVE_ABI, .size = sizeof(*output), .handle = 1,
        .writable = (uint64_t)(uintptr_t)address, .executable = (uint64_t)(uintptr_t)address,
        .capacity = capacity};
    return HL_NATIVE_OK;
}

static hl_native_status release(void *opaque, hl_native_handle handle) {
    executable_memory *memory = opaque;
    if (handle != 1 || memory->address == NULL) return HL_NATIVE_ARGUMENT;
    if (munmap(memory->address, (size_t)memory->capacity) != 0) return HL_NATIVE_PLATFORM;
    memory->address = NULL;
    return HL_NATIVE_OK;
}

static hl_native_status publish(void *opaque, hl_native_handle handle, uint64_t offset, uint64_t size) {
    executable_memory *memory = opaque;
    return handle == 1 && offset <= memory->capacity && size <= memory->capacity - offset
        ? HL_NATIVE_OK : HL_NATIVE_ARGUMENT;
}

static hl_native_status repair(void *opaque, hl_native_mapping *mapping, uint32_t preserve) {
    executable_memory *memory = opaque;
    (void)preserve;
    return mapping->handle == 1 && mapping->writable == (uint64_t)(uintptr_t)memory->address
        ? HL_NATIVE_OK : HL_NATIVE_ARGUMENT;
}

static hl_native_status write_begin(void *opaque) {
    executable_memory *memory = opaque;
    return mprotect(memory->address, (size_t)memory->capacity, PROT_READ | PROT_WRITE) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static hl_native_status write_end(void *opaque) {
    executable_memory *memory = opaque;
    __builtin___clear_cache((char *)memory->address, (char *)memory->address + memory->capacity);
    return mprotect(memory->address, (size_t)memory->capacity, PROT_READ | PROT_EXEC) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static hl_native_executor *create_flags(executable_memory *host, uint32_t flags) {
    const hl_native_memory memory = {.abi = HL_NATIVE_ABI, .size = sizeof(memory), .context = host,
        .reserve = reserve, .release = release, .publish = publish, .repair = repair,
        .write_begin = write_begin, .write_end = write_end};
    const hl_native_config config = {.abi = HL_NATIVE_ABI, .size = sizeof(config),
        .capacity = 64u << 20, .alignment = 4096, .flags = flags,
        .memory = &memory};
    hl_native_executor *executor = NULL;
    return hl_native_create(&config, &executor) == HL_NATIVE_OK ? executor : NULL;
}

static hl_native_executor *create(executable_memory *host) {
    return create_flags(host, HL_NATIVE_DIAGNOSTICS);
}

static int dirty_overflow_continue_configuration(void) {
    executable_memory host = {0};
    hl_native_executor *executor = create_flags(&host, 0);
    CHECK(executor != NULL && executor->dirty_overflow_continue == 1);
    CHECK(hl_native_destroy(executor) == HL_NATIVE_OK && host.address == NULL);

    executor = create_flags(&host, HL_NATIVE_A64_DIRTY_OVERFLOW_CONTINUE);
    CHECK(executor != NULL && executor->dirty_overflow_continue == 1);
    CHECK(hl_native_destroy(executor) == HL_NATIVE_OK && host.address == NULL);

    executor = create_flags(&host, HL_NATIVE_A64_DIRTY_OVERFLOW_EXIT);
    CHECK(executor != NULL && executor->dirty_overflow_continue == 0);
    CHECK(hl_native_destroy(executor) == HL_NATIVE_OK && host.address == NULL);

    executor = create_flags(&host, HL_NATIVE_A64_DIRTY_OVERFLOW_CONTINUE |
                                   HL_NATIVE_A64_DIRTY_OVERFLOW_EXIT);
    CHECK(executor == NULL && host.address == NULL);
    return 0;
}

static int publication(void) {
    executable_memory host = {0};
    hl_native_executor *executor = create(&host);
    CHECK(executor != NULL);
    const hl_native_change replace = {.abi = HL_NATIVE_ABI, .size = sizeof(replace),
        .kind = HL_NATIVE_REPLACE, .mapping_epoch = 7};
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);

    const uint32_t exit_word = UINT32_C(0x14000000);
    const hl_native_provenance left_map = {.code_offset = 0, .code_size = 4, .guest = 0x1000};
    const hl_native_provenance right_map = {.code_offset = 0, .code_size = 4, .guest = 0x2000};
    const hl_native_relocation left_link = {.code_offset = 0, .target_guest = 0x2000,
        .target_instruction_epoch = 3, .target_epoch_known = 1, .expected = exit_word};
    const hl_native_relocation right_link = {.code_offset = 0, .target_guest = 0x1000,
        .target_instruction_epoch = 3, .target_epoch_known = 1, .expected = exit_word};
    const hl_native_translation_key left = {0x1000, 7, 3, 0x1000, 0x1004, 0, 0, 0, 0, 0};
    const hl_native_translation_key right = {0x2000, 7, 3, 0x2000, 0x2004, 0, 0, 0, 0, 0};
    const hl_native_emission left_emission = {.bytes = (const uint8_t *)&exit_word,
        .size = sizeof(exit_word), .provenance = &left_map, .provenance_count = 1,
        .relocations = &left_link, .relocation_count = 1, .cycle_safe = 1};
    const hl_native_emission right_emission = {.bytes = (const uint8_t *)&exit_word,
        .size = sizeof(exit_word), .provenance = &right_map, .provenance_count = 1,
        .relocations = &right_link, .relocation_count = 1, .cycle_safe = 1};
    hl_native_code code;
    CHECK(hl_native_translation_publish(executor, &left, &left_emission) == HL_NATIVE_OK);
    CHECK(hl_native_translation_publish(executor, &right, &right_emission) == HL_NATIVE_OK);
    CHECK(hl_native_translation_lookup(executor, &left, &code) == HL_NATIVE_HIT);
    CHECK(*(const uint32_t *)code.entry != exit_word && code.cycle_safe == 1);
    CHECK(hl_native_translation_lookup(executor, &right, &code) == HL_NATIVE_HIT);
    CHECK(*(const uint32_t *)code.entry != exit_word && code.cycle_safe == 1);

    const hl_native_change reset = {.abi = HL_NATIVE_ABI, .size = sizeof(reset),
        .kind = HL_NATIVE_REPLACE, .mapping_epoch = 8};
    CHECK(hl_native_changed(executor, &reset, 1) == HL_NATIVE_OK);
    hl_native_translation_key mixed_left = left, mixed_right = right;
    mixed_left.mapping_incarnation = mixed_right.mapping_incarnation = 8;
    hl_native_emission unsafe_right = right_emission;
    unsafe_right.cycle_safe = 0;
    CHECK(hl_native_translation_publish(executor, &mixed_left, &left_emission) == HL_NATIVE_OK);
    CHECK(hl_native_translation_publish(executor, &mixed_right, &unsafe_right) == HL_NATIVE_OK);
    CHECK(hl_native_translation_lookup(executor, &mixed_left, &code) == HL_NATIVE_HIT);
    const uint32_t *mixed_left_word = code.entry;
    CHECK(hl_native_translation_lookup(executor, &mixed_right, &code) == HL_NATIVE_HIT);
    const uint32_t *mixed_right_word = code.entry;
    CHECK((*mixed_left_word == exit_word) != (*mixed_right_word == exit_word));

    const hl_native_change branch_reset = {.abi = HL_NATIVE_ABI, .size = sizeof(branch_reset),
        .kind = HL_NATIVE_REPLACE, .mapping_epoch = 9};
    CHECK(hl_native_changed(executor, &branch_reset, 1) == HL_NATIVE_OK);
    const hl_native_translation_key branch_source = {0x3000, 9, 4, 0x3000, 0x3004, 0, 0, 0, 0, 0};
    const hl_native_translation_key branch_target = {0x4000, 9, 4, 0x4000, 0x4004, 0, 0, 0, 0, 0};
    const hl_native_translation_key branch_unsafe = {0x5000, 9, 4, 0x5000, 0x5004, 0, 0, 0, 0, 0};
    const hl_native_provenance branch_source_map = {
        .code_offset = 0, .code_size = 4, .guest = 0x3000};
    const hl_native_provenance branch_target_map = {
        .code_offset = 0, .code_size = 4, .guest = 0x4000};
    const hl_native_provenance branch_unsafe_map = {
        .code_offset = 0, .code_size = 4, .guest = 0x5000};
    const hl_native_relocation source_target = {.code_offset = 0, .target_guest = 0x4000,
        .target_instruction_epoch = 4, .target_epoch_known = 1, .expected = exit_word};
    const hl_native_relocation unsafe_source = {.code_offset = 0, .target_guest = 0x3000,
        .target_instruction_epoch = 4, .target_epoch_known = 1, .expected = exit_word};
    const hl_native_relocation target_paths[] = {
        {.code_offset = 0, .target_guest = 0x3000, .target_instruction_epoch = 4,
         .target_epoch_known = 1, .expected = exit_word},
        {.code_offset = 4, .target_guest = 0x5000, .target_instruction_epoch = 4,
         .target_epoch_known = 1, .expected = exit_word},
    };
    const uint32_t target_words[] = {exit_word, exit_word};
    const hl_native_emission branch_source_emission = {.bytes = (const uint8_t *)&exit_word,
        .size = sizeof(exit_word), .provenance = &branch_source_map, .provenance_count = 1,
        .relocations = &source_target, .relocation_count = 1, .cycle_safe = 1};
    const hl_native_emission branch_unsafe_emission = {.bytes = (const uint8_t *)&exit_word,
        .size = sizeof(exit_word), .provenance = &branch_unsafe_map, .provenance_count = 1,
        .relocations = &unsafe_source, .relocation_count = 1};
    const hl_native_emission branch_target_emission = {.bytes = (const uint8_t *)target_words,
        .size = sizeof(target_words), .provenance = &branch_target_map, .provenance_count = 1,
        .relocations = target_paths, .relocation_count = 2, .cycle_safe = 1};
    CHECK(hl_native_translation_publish(executor, &branch_source, &branch_source_emission) == HL_NATIVE_OK);
    CHECK(hl_native_translation_publish(executor, &branch_unsafe, &branch_unsafe_emission) == HL_NATIVE_OK);
    CHECK(hl_native_translation_publish(executor, &branch_target, &branch_target_emission) == HL_NATIVE_OK);
    CHECK(hl_native_translation_lookup(executor, &branch_source, &code) == HL_NATIVE_HIT);
    CHECK(*(const uint32_t *)code.entry == exit_word);

    CHECK(hl_native_destroy(executor) == HL_NATIVE_OK && host.address == NULL);
    return 0;
}

static int execution(void) {
#if !defined(__aarch64__)
    return 0;
#else
    executable_memory host = {0};
    hl_native_executor *executor = create(&host);
    CHECK(executor != NULL);
    const hl_native_change replace = {.abi = HL_NATIVE_ABI, .size = sizeof(replace),
        .kind = HL_NATIVE_REPLACE, .mapping_epoch = 11};
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);
    static _Alignas(16) uint8_t stack[4096];
    uint64_t stack_first = (uint64_t)(uintptr_t)stack;
    uint64_t stack_last = (uint64_t)(uintptr_t)(stack + sizeof(stack));
    const hl_a64_view view = {stack_first, stack_last, stack_first, 11,
                              HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE,
                              HL_NATIVE_WRITE_EXACT, 0};
    const hl_a64_projection projection = {&view, 1, 11, 0};
    hl_native_aarch64_cpu state = {0};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_AARCH64, .state.aarch64 = &state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_AARCH64, .mapping_epoch = 11, .projection = &projection};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};

    const uint32_t self_words[] = {UINT32_C(0xf1000400), UINT32_C(0x54ffffe1)};
    const hl_native_source_span self_span = {
        0x3000, (const uint8_t *)self_words, sizeof(self_words), 11, 5};
    const hl_native_source self_source = {&self_span, 1, 11, 5};
    request.source = &self_source;
    request.budget = 10;
    state.program = 0x3000;
    state.stack = stack_last;
    state.registers[0] = 100;
    hl_native_diagnostics before = {.abi = HL_NATIVE_ABI, .size = sizeof(before)}, after = before;
    CHECK(hl_native_diagnose(executor, &before) == HL_NATIVE_OK);
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 10 && state.registers[0] == 95);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    /* The first unresolved self edge returns once before relocation closes
     * the cycle; only the remaining iterations stay inside native code. */
    CHECK(after.boundary_branch == before.boundary_branch + 1 &&
          after.boundary_yield == before.boundary_yield);
    request.budget = 10;
    state.interrupt = 1;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_INTERRUPT && state.executed == 0 && state.budget == 10);
    state.interrupt = 0;

    const uint32_t pair_words[] = {
        UINT32_C(0x14000002), UINT32_C(0xd503201f), UINT32_C(0x17fffffe)};
    const hl_native_source_span pair_span = {
        0x4000, (const uint8_t *)pair_words, sizeof(pair_words), 11, 6};
    const hl_native_source pair_source = {&pair_span, 1, 11, 6};
    request.source = &pair_source;
    request.budget = 6;
    state.program = 0x4000;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 6);
    before = (hl_native_diagnostics){.abi = HL_NATIVE_ABI, .size = sizeof(before)};
    after = before;
    CHECK(hl_native_diagnose(executor, &before) == HL_NATIVE_OK);
    request.budget = 6;
    state.program = 0x4000;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 6);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    CHECK(after.boundary_branch == before.boundary_branch + 1);

    const hl_native_change invalidate = {.abi = HL_NATIVE_ABI, .size = sizeof(invalidate),
        .kind = HL_NATIVE_INVALIDATE, .first = 0x4008, .last = 0x400c, .mapping_epoch = 11};
    before = (hl_native_diagnostics){.abi = HL_NATIVE_ABI, .size = sizeof(before)};
    after = before;
    CHECK(hl_native_diagnose(executor, &before) == HL_NATIVE_OK);
    CHECK(hl_native_changed(executor, &invalidate, 1) == HL_NATIVE_OK);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    CHECK(after.invalidations == before.invalidations + 1);
    before = after;
    request.budget = 6;
    state.program = 0x4000;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 6);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    /* Direct-successor folding compiles the self-edge block into one cache entry, so the re-run
     * costs a single dispatcher round trip. */
    CHECK(after.boundary_branch == before.boundary_branch + 1);

    CHECK(hl_native_destroy(executor) == HL_NATIVE_OK && host.address == NULL);
    return 0;
#endif
}

static int branch_diagnostics(void) {
#if !defined(__aarch64__)
    return 0;
#else
    executable_memory host = {0};
    hl_native_executor *executor = create(&host);
    CHECK(executor != NULL);
    const hl_native_change replace = {.abi = HL_NATIVE_ABI, .size = sizeof(replace),
        .kind = HL_NATIVE_REPLACE, .mapping_epoch = 21};
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);
    static _Alignas(16) uint8_t stack[4096];
    uint32_t sequential_words[33];
    for (size_t index = 0; index < 32; index++) sequential_words[index] = UINT32_C(0xd503201f);
    sequential_words[32] = UINT32_C(0xd4000001);
    const hl_native_source_span sequential_span = {
        0x5000, (const uint8_t *)sequential_words, sizeof(sequential_words), 21, 7};
    const hl_native_source sequential_source = {&sequential_span, 1, 21, 7};
    hl_native_aarch64_cpu state = {.program = 0x5000,
        .stack = (uint64_t)(uintptr_t)(stack + sizeof(stack))};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_AARCH64, .state.aarch64 = &state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_AARCH64, .mapping_epoch = 21,
        .budget = 33, .source = &sequential_source};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    hl_native_diagnostics before = {.abi = HL_NATIVE_ABI, .size = sizeof(before)};
    hl_native_diagnostics after = before;
    CHECK(hl_native_diagnose(executor, &before) == HL_NATIVE_OK);
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && state.executed == 33);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    /* The exit pc is the block's source_last, so this is exhaustion even though the
     * block also carries a relocation. */
    CHECK(after.a64_branch_exhaustion - before.a64_branch_exhaustion == 1 &&
          after.a64_branch_cold_relocation - before.a64_branch_cold_relocation == 0 &&
          after.a64_branch_nonrelocatable - before.a64_branch_nonrelocatable == 0 &&
          after.a64_branch_unidentified - before.a64_branch_unidentified == 0);
    CHECK(after.a64_branch_sample_pc == 0x5080 &&
          after.a64_branch_sample_source_first == 0x5000 &&
          after.a64_branch_sample_source_last == 0x5080 &&
          after.a64_branch_sample_form == HL_NATIVE_A64_BRANCH_FORM_EXHAUSTION);
    before = after;
    state.program = 0x5000;
    request.budget = 33;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && state.executed == 33);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    CHECK(after.boundary_branch == before.boundary_branch &&
          after.a64_branch_cold_relocation == before.a64_branch_cold_relocation);

    const uint32_t branch_words[] = {UINT32_C(0x14000001), UINT32_C(0xd503201f)};
    const hl_native_source_span branch_span = {
        0x6000, (const uint8_t *)branch_words, sizeof(branch_words), 22, 8};
    const hl_native_source branch_source = {&branch_span, 1, 22, 8};
    const hl_native_change next = {.abi = HL_NATIVE_ABI, .size = sizeof(next),
        .kind = HL_NATIVE_REPLACE, .mapping_epoch = 22};
    CHECK(hl_native_changed(executor, &next, 1) == HL_NATIVE_OK);
    before = after;
    state.program = 0x6000;
    request.mapping_epoch = 22;
    request.budget = 2;
    request.source = &branch_source;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 2);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    CHECK(after.a64_branch_exhaustion - before.a64_branch_exhaustion == 2 &&
          after.a64_branch_cold_relocation - before.a64_branch_cold_relocation == 0 &&
          after.a64_branch_nonrelocatable - before.a64_branch_nonrelocatable == 0 &&
          after.a64_branch_unidentified - before.a64_branch_unidentified == 0);
    CHECK(after.a64_branch_sample_pc == 0x5080 &&
          after.a64_branch_sample_source_first == 0x5000 &&
          after.a64_branch_sample_source_last == 0x5080 &&
          after.a64_branch_sample_form == HL_NATIVE_A64_BRANCH_FORM_EXHAUSTION);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_OK);
    CHECK(hl_native_after_fork(executor, 1) == HL_NATIVE_OK);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK &&
          after.a64_branch_sample_pc == 0x5080 &&
          after.a64_branch_sample_form == HL_NATIVE_A64_BRANCH_FORM_EXHAUSTION);
    CHECK(hl_native_destroy(executor) == HL_NATIVE_OK && host.address == NULL);

    executable_memory cold_host = {0};
    executor = create(&cold_host);
    CHECK(executor != NULL);
    const hl_native_change cold_replace = {.abi = HL_NATIVE_ABI, .size = sizeof(cold_replace),
        .kind = HL_NATIVE_REPLACE, .mapping_epoch = 22};
    CHECK(hl_native_changed(executor, &cold_replace, 1) == HL_NATIVE_OK);
    state.program = 0x6000;
    request.mapping_epoch = 22;
    request.budget = 2;
    request.source = &branch_source;
    before = (hl_native_diagnostics){.abi = HL_NATIVE_ABI, .size = sizeof(before)};
    after = before;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 2);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    CHECK(after.a64_branch_exhaustion == 2 &&
          after.a64_branch_sample_pc == 0x6004 &&
          after.a64_branch_sample_source_first == 0x6000 &&
          after.a64_branch_sample_source_last == 0x6004 &&
          after.a64_branch_sample_form == HL_NATIVE_A64_BRANCH_FORM_EXHAUSTION);
    state.program = 0x6000;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    before = after;
    state.program = 0x6000;
    request.budget = 1;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 1);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    CHECK(after.a64_branch_unidentified == before.a64_branch_unidentified);
    request.budget = 2;

    hl_native_code hot_code;
    cache_entry *hot_entry = NULL;
    for (uint32_t index = 0; index < executor->cache->capacity; index++)
        if (executor->cache->entries[index].state == ENTRY_LIVE &&
            executor->cache->entries[index].guest == 0x6000)
            hot_entry = &executor->cache->entries[index];
    CHECK(hot_entry != NULL);
    hot_code = (hl_native_code){
        .entry = (void *)(uintptr_t)(executor->cache->arena->mapping.executable + hot_entry->code_offset),
        .body = (void *)(uintptr_t)(executor->cache->arena->mapping.executable + hot_entry->body_offset),
        .code_size = hot_entry->code_size};
    resolved_relocation *hot_edge = NULL;
    for (uint32_t index = 0; index < executor->cache->resolved_count; index++)
        if (executor->cache->resolved[index].source_guest == 0x6000)
            hot_edge = &executor->cache->resolved[index];
    CHECK(hot_edge != NULL);
    void *hot_admission = (void *)(uintptr_t)(executor->cache->arena->mapping.executable +
        hot_edge->source_code_offset + hot_edge->relocation.code_offset);
    const hl_native_aarch64_cpu saved_state = state;
    state.program = 0x6000;
    state.budget = 2;
    state.registers[16] = (uint64_t)(uintptr_t)hot_admission;
    state.interrupt = 1;
    state.execution_identity = 0;
    hl_native_aarch64_enter(&state, enter_register_16);
    CHECK(state.reason == HL_NATIVE_EXIT_BRANCH && state.program == 0x6004 &&
          state.execution_identity > (uint64_t)(uintptr_t)hot_code.entry &&
          state.execution_identity - 1 < (uint64_t)(uintptr_t)hot_code.entry + hot_code.code_size);
    state = saved_state;

    hl_native_interrupt_token *interrupt_token = NULL;
    CHECK(hl_native_interrupt_create(&interrupt_token) == HL_NATIVE_OK);
    CHECK(hl_native_interrupt_set(interrupt_token, 1) == HL_NATIVE_OK);
    state.program = 0x6000;
    state.budget = 2;
    state.registers[16] = (uint64_t)(uintptr_t)hot_admission;
    state.interrupt_token = (uint64_t)(uintptr_t)interrupt_token;
    state.execution_identity = 0;
    hl_native_aarch64_enter(&state, enter_register_16);
    CHECK(state.reason == HL_NATIVE_EXIT_BRANCH && state.program == 0x6004 &&
          state.execution_identity > (uint64_t)(uintptr_t)hot_code.entry &&
          state.execution_identity - 1 < (uint64_t)(uintptr_t)hot_code.entry + hot_code.code_size);
    state = saved_state;
    hl_native_interrupt_destroy(interrupt_token);

    const hl_native_change invalidate_target = {.abi = HL_NATIVE_ABI,
        .size = sizeof(invalidate_target), .kind = HL_NATIVE_INVALIDATE,
        .first = 0x6004, .last = 0x6008, .mapping_epoch = 22};
    CHECK(hl_native_changed(executor, &invalidate_target, 1) == HL_NATIVE_OK);
    before = after;
    state.program = 0x6000;
    request.budget = 2;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 2);
    state.program = 0x6000;
    request.budget = 1;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 1);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    CHECK(after.a64_branch_unidentified == before.a64_branch_unidentified);
    request.budget = 2;
    const hl_native_change cold_reset = {.abi = HL_NATIVE_ABI, .size = sizeof(cold_reset),
        .kind = HL_NATIVE_REPLACE, .mapping_epoch = 23};
    CHECK(hl_native_changed(executor, &cold_reset, 1) == HL_NATIVE_OK);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_OK);
    CHECK(hl_native_after_fork(executor, 1) == HL_NATIVE_OK);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK &&
          after.a64_branch_sample_pc == 0x6004 &&
          after.a64_branch_sample_source_first == 0x6000 &&
          after.a64_branch_sample_source_last == 0x6004 &&
          after.a64_branch_sample_form == HL_NATIVE_A64_BRANCH_FORM_EXHAUSTION);
    CHECK(hl_native_destroy(executor) == HL_NATIVE_OK && cold_host.address == NULL);

    /* A backward branch leaves the block before its source_last, so this exit is a
     * cold relocation rather than exhaustion. */
    executable_memory back_host = {0};
    executor = create(&back_host);
    CHECK(executor != NULL);
    const hl_native_change back_replace = {.abi = HL_NATIVE_ABI, .size = sizeof(back_replace),
        .kind = HL_NATIVE_REPLACE, .mapping_epoch = 24};
    CHECK(hl_native_changed(executor, &back_replace, 1) == HL_NATIVE_OK);
    const uint32_t back_words[] = {UINT32_C(0x17ffffff), UINT32_C(0xd503201f)};
    const hl_native_source_span back_span = {
        0x7000, (const uint8_t *)back_words, sizeof(back_words), 24, 9};
    const hl_native_source back_source = {&back_span, 1, 24, 9};
    state.program = 0x7000;
    request.mapping_epoch = 24;
    request.budget = 2;
    request.source = &back_source;
    before = (hl_native_diagnostics){.abi = HL_NATIVE_ABI, .size = sizeof(before)};
    after = before;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    CHECK(after.a64_branch_cold_relocation == 1 && after.a64_branch_exhaustion == 0 &&
          after.a64_branch_sample_pc == 0x6ffc &&
          after.a64_branch_sample_source_first == 0x7000 &&
          after.a64_branch_sample_source_last == 0x7004 &&
          after.a64_branch_sample_form == HL_NATIVE_A64_BRANCH_FORM_COLD_RELOCATION);
    CHECK(hl_native_destroy(executor) == HL_NATIVE_OK && back_host.address == NULL);
    return 0;
#endif
}

static int diagnostics_off_cycle_closure(void) {
#if !defined(__aarch64__)
    return 0;
#else
    executable_memory host = {0};
    hl_native_executor *executor = create_flags(&host, 0);
    CHECK(executor != NULL);
    const hl_native_change replace = {.abi = HL_NATIVE_ABI, .size = sizeof(replace),
        .kind = HL_NATIVE_REPLACE, .mapping_epoch = 31};
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);
    const uint32_t word = UINT32_C(0x14000000);
    const hl_native_source_span span = {0x7000, (const uint8_t *)&word, sizeof(word), 31, 9};
    const hl_native_source source = {&span, 1, 31, 9};
    hl_native_aarch64_cpu state = {.program = 0x7000};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_AARCH64, .state.aarch64 = &state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_AARCH64, .mapping_epoch = 31, .budget = 3, .source = &source};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.program == 0x7000 && state.executed == 3);
    CHECK(hl_native_destroy(executor) == HL_NATIVE_OK && host.address == NULL);
    return 0;
#endif
}

#if defined(__aarch64__) && defined(__linux__)
typedef struct aperture_fault_owner {
    uint64_t generation;
} aperture_fault_owner;

static uint32_t aperture_fault_publish(void *opaque, const hl_native_fault_scope *scope) {
    aperture_fault_owner *owner = opaque;
    return hl_native_fault_thread_publish(scope, &owner->generation);
}

static void aperture_fault_unpublish(void *opaque, const hl_native_fault_scope *scope) {
    aperture_fault_owner *owner = opaque;
    if (owner->generation == 0 ||
        hl_native_fault_thread_unpublish(scope, owner->generation) != HL_NATIVE_OK)
        abort();
    owner->generation = 0;
}

static void aperture_state(hl_native_aarch64_cpu *state, uint64_t pc,
                           uint64_t address, uint64_t value) {
    memset(state, 0, sizeof(*state));
    state->program = pc;
    state->registers[0] = address;
    state->registers[1] = value;
    state->dirty_first = UINT64_MAX;
}
#endif

static int fixed_aperture_execution(void) {
#if !defined(__aarch64__) || !defined(__linux__)
    return 0;
#else
    const size_t aperture_bytes = (size_t)HL_NATIVE_APERTURE_MAX_BYTES;
    uint8_t *aperture = mmap(NULL, aperture_bytes, PROT_NONE,
                             MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
    CHECK(aperture != MAP_FAILED);
    CHECK(mprotect(aperture + 0x2000, 0x2000, PROT_READ | PROT_WRITE) == 0);
    CHECK(mprotect(aperture + 0x4000, 0x1000, PROT_READ) == 0);
    CHECK(mprotect(aperture + 0x6000, 0x2000, PROT_READ | PROT_WRITE) == 0);
    CHECK(hl_native_fault_process_acquire() == HL_NATIVE_OK);
    CHECK(hl_native_fault_thread_attach() == HL_NATIVE_OK);

    executable_memory host = {0};
    hl_native_executor *executor = create_flags(
        &host, HL_NATIVE_DIAGNOSTICS | HL_NATIVE_A64_FIXED_APERTURE);
    CHECK(executor != NULL);
    hl_native_direct_authority authority = {
        .abi = HL_NATIVE_ABI,
        .size = sizeof(authority),
        .permissions = HL_NATIVE_ACCESS_APERTURE,
        .guest_first = 0,
        .guest_last = HL_NATIVE_APERTURE_MAX_BYTES,
        .host_first = (uint64_t)(uintptr_t)aperture,
        .mapping_incarnation = 77,
        .mapping_generation = 3,
        .instruction_generation = 9,
    };
    hl_native_direct_token *token = NULL;
    CHECK(hl_native_direct_register(executor, &authority, &token) == HL_NATIVE_OK);
    hl_native_projection_view view = {
        .guest_first = authority.guest_first,
        .guest_last = authority.guest_last,
        .host_first = authority.host_first,
        .mapping_incarnation = authority.mapping_incarnation,
        .permissions = HL_NATIVE_ACCESS_APERTURE,
        .write_policy = HL_NATIVE_WRITE_EXACT,
    };
    hl_native_projection projection = {&view, 1, authority.mapping_incarnation, 0};
    const uint32_t store_words[] = {UINT32_C(0xf9000001), UINT32_C(0xd4000001)};
    const hl_native_source_span store_span = {
        0x10000, (const uint8_t *)store_words, sizeof(store_words), 77, 9};
    const hl_native_source store_source = {&store_span, 1, 77, 9};
    const uint32_t load_words[] = {UINT32_C(0xf9400002), UINT32_C(0xd4000001)};
    const hl_native_source_span load_span = {
        0x11000, (const uint8_t *)load_words, sizeof(load_words), 77, 9};
    const hl_native_source load_source = {&load_span, 1, 77, 9};
    aperture_fault_owner faults = {0};
    hl_native_run_request request = {
        .abi = HL_NATIVE_ABI,
        .size = sizeof(request),
        .architecture = HL_NATIVE_AARCH64,
        .mapping_epoch = 77,
        .budget = 2,
        .source = &store_source,
        .projection = &projection,
        .fault_context = &faults,
        .fault_publish = aperture_fault_publish,
        .fault_unpublish = aperture_fault_unpublish,
        .memory_mode = HL_NATIVE_MEMORY_APERTURE,
        .authority_generation = hl_native_direct_generation(executor, token),
        .direct_token = token,
        .authority_identity = hl_native_direct_identity(executor, token),
    };
    hl_native_aarch64_cpu state;
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_AARCH64, .state.aarch64 = &state};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    const uint64_t stored = UINT64_C(0x123456789abcdef0);

    /* The second run is a cache hit but must execute and count the dynamic
     * store again. Pretend an exact load resolver left a narrow unrelated view;
     * the aperture store must restore its authenticated journal owner first. */
    for (unsigned iteration = 0; iteration < 2; ++iteration) {
        aperture_state(&state, 0x10000, 0x2000, stored + iteration);
        state.memory_first = 0x4000;
        state.memory_last = 0x5000;
        state.memory_delta = (uint64_t)(uintptr_t)aperture;
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL);
        CHECK(*(uint64_t *)(void *)(aperture + 0x2000) == stored + iteration);
        CHECK(state.dirty_view_first == 0 &&
              state.dirty_view_last == HL_NATIVE_APERTURE_MAX_BYTES &&
              state.dirty_first == 0x2000 && state.dirty_last == 0x2008 &&
              state.dirty_overflow == 0);
    }
    hl_native_diagnostics diagnostics = {.abi = HL_NATIVE_ABI, .size = sizeof(diagnostics)};
    CHECK(hl_native_diagnose(executor, &diagnostics) == HL_NATIVE_OK);
    CHECK(diagnostics.a64_aperture_scalar_attempts == 2 &&
          diagnostics.a64_aperture_hits == 2);

    /* Loads from both host-readable stand-ins for guest X-only and W-only
     * pages remain on the exact resolver path. Without a resolver they return
     * fallback and never become aperture hits. */
    request.source = &load_source;
    for (uint64_t address = 0x3000; address <= 0x4000; address += 0x1000) {
        aperture_state(&state, 0x11000, address, 0);
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK);
    }
    request.source = &store_source;

    /* Same-page/range checks happen before the fixed transform. Both backing
     * pages are writable, so only the 4KiB-cross guard can reject this store. */
    aperture_state(&state, 0x10000, 0x6ffc, stored);
    memset(aperture + 0x6ffc, 0, sizeof(stored));
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK && state.fault_address == 0x6ffc);
    CHECK(*(uint32_t *)(void *)(aperture + 0x6ffc) == 0);
    aperture_state(&state, 0x10000, HL_NATIVE_APERTURE_MAX_BYTES, stored);
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK &&
          state.fault_address == HL_NATIVE_APERTURE_MAX_BYTES);

    /* Host protection is the store authority. Read-only and hole faults are
     * recovered through normal provenance, with no journal commit or hit. */
    for (uint64_t address = 0x4000; address <= 0x5000; address += 0x1000) {
        aperture_state(&state, 0x10000, address, stored);
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_FAULT && state.fault_address == address);
        CHECK(state.dirty_first == UINT64_MAX && state.dirty_count == 0);
    }

    /* A full exact journal refuses before the first unjournalable store. */
    aperture_state(&state, 0x10000, 0x3000, stored);
    state.dirty_view_first = 0;
    state.dirty_view_last = HL_NATIVE_APERTURE_MAX_BYTES;
    state.dirty_first = 0x8000;
    state.dirty_last = 0x8008;
    state.dirty_count = 16;
    for (uint64_t index = 0; index < 16; ++index) {
        state.dirty_records[index][0] = 0;
        state.dirty_records[index][1] = HL_NATIVE_APERTURE_MAX_BYTES;
        state.dirty_records[index][2] = 0x10000 + index * 0x1000;
        state.dirty_records[index][3] = state.dirty_records[index][2] + 8;
    }
    *(uint64_t *)(void *)(aperture + 0x3000) = 0;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_EPOCH);
    CHECK(*(uint64_t *)(void *)(aperture + 0x3000) == 0);

    CHECK(hl_native_diagnose(executor, &diagnostics) == HL_NATIVE_OK);
    CHECK(diagnostics.a64_aperture_scalar_attempts == 9 &&
          diagnostics.a64_aperture_hits == 2 &&
          diagnostics.a64_aperture_hits <= diagnostics.a64_aperture_scalar_attempts);
    CHECK(hl_native_direct_unregister(executor, token) == HL_NATIVE_OK);
    CHECK(hl_native_destroy(executor) == HL_NATIVE_OK && host.address == NULL);
    CHECK(hl_native_fault_thread_detach() == HL_NATIVE_OK);
    CHECK(hl_native_fault_process_release() == HL_NATIVE_OK);
    CHECK(munmap(aperture, aperture_bytes) == 0);
    return 0;
#endif
}

int main(void) {
    return dirty_overflow_continue_configuration() != 0 ||
           branch_diagnostics() != 0 || diagnostics_off_cycle_closure() != 0 ||
           publication() != 0 || execution() != 0 || fixed_aperture_execution() != 0;
}
