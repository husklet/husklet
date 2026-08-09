#define _GNU_SOURCE

#include "../src/arch/aarch64/guard.h"
#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/stub.h"
#include "../src/arch/aarch64/projection.h"
#include "../include/executor.h"

#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "dirty_bound:%d: %s\n", __LINE__, #x); return 1; } } while (0)

#define CAPACITY \
    ((uint64_t)(sizeof(((hl_native_aarch64_cpu *)0)->dirty_records) / \
                sizeof(((hl_native_aarch64_cpu *)0)->dirty_records[0])))

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

static size_t emit_archive(hl_a64_assembler *assembler, uint64_t pc, uint64_t marker) {
    size_t offset = hl_a64_assembler_size(assembler);
    hl_a64_stub_prologue(assembler);
    hl_a64_ldr(assembler, 16, 28, 16 * 8);
    hl_a64_guard reserve = {0};
    hl_a64_guard_write_begin(assembler, 8, pc, &reserve);
    hl_a64_movconst(assembler, 17, marker);
    hl_a64_str(assembler, 17, 28, (int)offsetof(hl_native_aarch64_cpu, read_token));
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return offset;
}

static void seed_overflow(hl_native_aarch64_cpu *cpu, uint8_t *data, uint64_t *stack) {
    memset(cpu, 0, sizeof(*cpu));
    cpu->stack = (uint64_t)(uintptr_t)(stack + 256);
    cpu->budget = 1024;
    cpu->memory_first = (uint64_t)(uintptr_t)data;
    cpu->memory_last = cpu->memory_first + 64;
    cpu->memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    cpu->dirty_first = cpu->memory_first;
    cpu->dirty_last = cpu->memory_first + 8;
    cpu->dirty_view_first = cpu->memory_first + 4096;
    cpu->dirty_view_last = cpu->memory_first + 8192;
    cpu->dirty_count = CAPACITY;
    cpu->registers[16] = cpu->memory_first + 16;
    cpu->read_token = UINT64_C(0x5ec0de5ec0de5ec0);
    cpu->read_incarnation = UINT64_C(0x1111111111111111);
    cpu->read_count = UINT64_C(0x2222222222222222);
    cpu->read_views[0][0] = UINT64_C(0x3333333333333333);
}

/* The post-store journal append is the last aarch64 site that grew the ring
 * without a capacity compare. dirty_records[16] is not padding: it lands on
 * read_token, so an unbounded append is an intra-object overwrite that ASan
 * cannot see. Drive the append alone, at capacity, on a disjoint view. */
int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    CHECK(offsetof(hl_native_aarch64_cpu, dirty_records) +
              sizeof(((hl_native_aarch64_cpu *)0)->dirty_records) ==
          offsetof(hl_native_aarch64_cpu, read_token));

    size_t capacity = (size_t)page * 4;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    hl_a64_assembler assembler;
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    CHECK(assembler.dirty_overflow_continue == 0);
    size_t written_offset = hl_a64_assembler_size(&assembler);
    /* x16 is stolen, so the prologue does not load it; the guard sequence
     * normally leaves the projected address there. */
    hl_a64_stub_prologue(&assembler);
    hl_a64_ldr(&assembler, 16, 28, 16 * 8);
    hl_a64_guard_written(&assembler, 8);
    hl_a64_stub_exit(&assembler, HL_NATIVE_EXIT_BRANCH, 0x4000);
    size_t default_offset = emit_archive(&assembler, 0x5000, UINT64_C(0xdedefa17dedefa17));
    assembler.dirty_overflow_continue = 1;
    size_t enabled_offset = emit_archive(&assembler, 0x6000, UINT64_C(0x5eed5eed5eed5eed));
    CHECK(hl_a64_assembler_ok(&assembler));
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    uint8_t data[64];
    memset(data, 0, sizeof(data));
    static uint64_t stack[512];
    hl_native_aarch64_cpu cpu;
    seed_overflow(&cpu, data, stack);
    /* A live interval owned by a different projection view: the view-identity
     * compare fails, which is exactly the disjoint path that appends. */

    execute(&cpu, code + written_offset);

    /* The ring is full, so the append must be refused and reported through the
     * established overflow flag rather than run off the end of the records. */
    CHECK(cpu.dirty_count == CAPACITY);
    CHECK(cpu.dirty_overflow == 1);
    CHECK(cpu.read_token == UINT64_C(0x5ec0de5ec0de5ec0));
    CHECK(cpu.read_incarnation == UINT64_C(0x1111111111111111));
    CHECK(cpu.read_count == UINT64_C(0x2222222222222222));
    CHECK(cpu.read_views[0][0] == UINT64_C(0x3333333333333333));

    seed_overflow(&cpu, data, stack);
    execute(&cpu, code + default_offset);
    CHECK(cpu.reason == HL_NATIVE_EXIT_EPOCH && cpu.program == 0x5000);
    CHECK(cpu.dirty_count == CAPACITY && cpu.dirty_overflow == 0);
    CHECK(cpu.read_token == UINT64_C(0x5ec0de5ec0de5ec0));

    seed_overflow(&cpu, data, stack);
    execute(&cpu, code + enabled_offset);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == 0x6004);
    CHECK(cpu.dirty_count == CAPACITY && cpu.dirty_overflow == 1);
    CHECK(cpu.dirty_first == UINT64_MAX && cpu.dirty_last == 0);
    CHECK(cpu.read_token == UINT64_C(0x5eed5eed5eed5eed));
    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
