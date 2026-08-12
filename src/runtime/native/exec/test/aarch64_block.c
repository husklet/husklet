#include "support.h"
#include "../src/arch/aarch64/block.h"
#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/projection.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "block:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    const uint32_t words[] = {
        0xd2800061u, /* movz x1,#3 */
        0x91001021u, /* add x1,x1,#4 */
        0xf94003e2u, /* ldr x2,[sp] */
        0x94000001u, /* bl pc+4 */
        0xd4000001u, /* svc #0 */
        0x00000000u, /* unsupported udf */
    };
    const uint64_t first = 0x1000;
    hl_a64_source_span span = {first, (const uint8_t *)words, sizeof(words), 1, 2};
    hl_a64_source source = {&span, 1, 1, 2};
    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 5;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    hl_a64_block_result built;
    CHECK(code != MAP_FAILED);
    for (size_t i = 0; i < 5; i++) {
        CHECK(hl_a64_block_build(&source, first + i * 4, code + i * page, (size_t)page, &built));
        CHECK(built.state == HL_A64_BLOCK_BUILT && built.source_first == first + i * 4);
        CHECK(built.source_last == first + i * 4 + 4 && built.provenance.guest == first + i * 4);
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);
    _Alignas(16) uint64_t stack[64] = {UINT64_C(0xfeedfacecafebeef)};
    hl_native_aarch64_cpu cpu = {0};
    cpu.stack = (uint64_t)(uintptr_t)stack;
    cpu.memory_first = (uint64_t)(uintptr_t)stack;
    cpu.memory_last = (uint64_t)(uintptr_t)(stack + 64);
    cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    for (size_t i = 0; i < 5; i++) execute(&cpu, code + i * page);
    CHECK(cpu.registers[1] == 7);
    CHECK(cpu.registers[2] == UINT64_C(0xfeedfacecafebeef));
    CHECK(cpu.registers[30] == 0x1010);
    CHECK(cpu.reason == HL_NATIVE_EXIT_SYSCALL && cpu.program == 0x1010);
    CHECK(munmap(code, capacity) == 0);

    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    hl_native_config config = test_config(&memory, 0);
    hl_native_executor *executor = NULL;
    hl_native_code cached;
    hl_a64_block_state state;
    uint8_t scratch[HL_A64_BLOCK_MAX_BYTES];
    hl_native_diagnostics before = {.abi = HL_NATIVE_ABI, .size = sizeof(before)};
    hl_native_diagnostics after = before;
    hl_native_lookup_context context = {0};
    hl_native_change replace = {.abi = HL_NATIVE_ABI, .size = sizeof(replace),
                                .kind = HL_NATIVE_REPLACE, .mapping_epoch = 1};
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);
    CHECK(hl_native_diagnose(executor, &before) == HL_NATIVE_OK);
    CHECK(hl_a64_block_cache_inner(executor, &context, &source, first, scratch,
                                   sizeof(scratch), &cached, &state) == HL_NATIVE_OK);
    CHECK(state == HL_A64_BLOCK_BUILT);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    CHECK(context.lookups == 2 && context.misses == 1 && context.hits == 1 &&
          context.epoch_rejections == 0);
    CHECK(after.cache_lookups == before.cache_lookups &&
          after.cache_misses == before.cache_misses && after.cache_hits == before.cache_hits);
    before = after;
    CHECK(hl_a64_block_cache(executor, &source, first, scratch, sizeof(scratch), &cached, &state) == HL_NATIVE_OK);
    CHECK(state == HL_A64_BLOCK_HIT);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    CHECK(after.cache_lookups == before.cache_lookups + 1 &&
          after.cache_hits == before.cache_hits + 1);
    CHECK(hl_native_diagnose(executor, &before) == HL_NATIVE_OK);
    CHECK(hl_a64_block_cache(executor, &source, first + 4, scratch, sizeof(scratch),
                             &cached, &state) == HL_NATIVE_OK);
    CHECK(state == HL_A64_BLOCK_BUILT && cached.entry != NULL &&
          cached.source_first == first + 4 && cached.source_last == first + 8);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    CHECK(after.cache_lookups == before.cache_lookups + 2 &&
          after.cache_misses == before.cache_misses + 1 &&
          after.cache_hits == before.cache_hits + 1 &&
          after.publications == before.publications + 1);
    context = (hl_native_lookup_context){0};
    CHECK(hl_native_diagnose(executor, &before) == HL_NATIVE_OK);
    CHECK(hl_a64_block_cache_inner(executor, &context, &source, first, scratch,
                                   sizeof(scratch), &cached, &state) == HL_NATIVE_OK);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    CHECK(state == HL_A64_BLOCK_HIT && context.lookups == 1 && context.hits == 1);
    CHECK(after.cache_lookups == before.cache_lookups && after.cache_hits == before.cache_hits);
    context = (hl_native_lookup_context){0};
    CHECK(hl_native_diagnose(executor, &before) == HL_NATIVE_OK);
    CHECK(hl_a64_block_cache_inner(executor, &context, &source, first + 20, scratch,
                                   sizeof(scratch), &cached, &state) == HL_NATIVE_OK);
    CHECK(state == HL_A64_BLOCK_FALLBACK);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    CHECK(context.lookups == 1 && context.misses == 1);
    CHECK(after.publications == before.publications && after.live_blocks == before.live_blocks);
    CHECK(after.cache_lookups == before.cache_lookups && after.cache_misses == before.cache_misses);

    context = (hl_native_lookup_context){0};
    CHECK(hl_native_diagnose(executor, &before) == HL_NATIVE_OK);
    CHECK(hl_a64_block_cache_inner(NULL, &context, &source, first, scratch,
                                   sizeof(scratch), &cached, &state) == HL_NATIVE_ARGUMENT);
    CHECK(hl_a64_block_cache_inner(executor, NULL, &source, first, scratch,
                                   sizeof(scratch), &cached, &state) == HL_NATIVE_ARGUMENT);
    CHECK(hl_a64_block_cache_inner(executor, &context, NULL, first, scratch,
                                   sizeof(scratch), &cached, &state) == HL_NATIVE_ARGUMENT);
    CHECK(hl_a64_block_cache_inner(executor, &context, &source, first, scratch,
                                   sizeof(scratch), NULL, &state) == HL_NATIVE_ARGUMENT);
    CHECK(hl_a64_block_cache_inner(executor, &context, &source, first, scratch,
                                   sizeof(scratch), &cached, NULL) == HL_NATIVE_ARGUMENT);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    CHECK(context.lookups == 0 && after.cache_lookups == before.cache_lookups &&
          after.cache_hits == before.cache_hits && after.cache_misses == before.cache_misses &&
          after.epoch_rejections == before.epoch_rejections);

    replace.mapping_epoch = 2;
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);
    context = (hl_native_lookup_context){0};
    CHECK(hl_native_diagnose(executor, &before) == HL_NATIVE_OK);
    CHECK(hl_a64_block_cache_inner(executor, &context, &source, first, scratch,
                                   sizeof(scratch), &cached, &state) == HL_NATIVE_STATE);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    CHECK(context.lookups == 1 && context.epoch_rejections == 1);
    CHECK(after.cache_lookups == before.cache_lookups &&
          after.epoch_rejections == before.epoch_rejections);
    context = (hl_native_lookup_context){UINT64_MAX, UINT64_MAX, UINT64_MAX, UINT64_MAX};
    CHECK(hl_a64_block_cache_inner(executor, &context, &source, first, scratch,
                                   sizeof(scratch), &cached, &state) == HL_NATIVE_STATE);
    CHECK(context.lookups == UINT64_MAX && context.hits == UINT64_MAX &&
          context.misses == UINT64_MAX && context.epoch_rejections == UINT64_MAX);
    hl_native_destroy(executor);
    return 0;
#endif
}
