#include "../src/arch/aarch64/guard.h"
#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/stub.h"
#include "../src/arch/aarch64/projection.h"
#include "../include/executor.h"

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
    /* x16 is stolen, so the prologue does not load it; the guard sequence
     * normally leaves the projected address there. */
    hl_a64_stub_prologue(&assembler);
    hl_a64_ldr(&assembler, 16, 28, 16 * 8);
    hl_a64_guard_written(&assembler, 8);
    hl_a64_stub_exit(&assembler, HL_NATIVE_EXIT_BRANCH, 0x4000);
    CHECK(hl_a64_assembler_ok(&assembler));
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    uint8_t data[64];
    memset(data, 0, sizeof(data));
    static uint64_t stack[512];
    hl_native_aarch64_cpu cpu;
    memset(&cpu, 0, sizeof(cpu));
    cpu.stack = (uint64_t)(uintptr_t)(stack + 256);
    cpu.budget = 1024;
    cpu.memory_first = (uint64_t)(uintptr_t)data;
    cpu.memory_last = cpu.memory_first + sizeof(data);
    cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    cpu.memory_delta = 0;
    /* A live interval owned by a different projection view: the view-identity
     * compare fails, which is exactly the disjoint path that appends. */
    cpu.dirty_first = cpu.memory_first;
    cpu.dirty_last = cpu.memory_first + 8;
    cpu.dirty_view_first = cpu.memory_first + 4096;
    cpu.dirty_view_last = cpu.memory_first + 8192;
    cpu.dirty_count = CAPACITY;
    cpu.registers[16] = cpu.memory_first + 16;
    cpu.read_token = UINT64_C(0x5ec0de5ec0de5ec0);
    cpu.read_incarnation = UINT64_C(0x1111111111111111);
    cpu.read_count = UINT64_C(0x2222222222222222);
    cpu.read_views[0][0] = UINT64_C(0x3333333333333333);

    execute(&cpu, code);

    /* The ring is full, so the append must be refused and reported through the
     * established overflow flag rather than run off the end of the records. */
    CHECK(cpu.dirty_count == CAPACITY);
    CHECK(cpu.dirty_overflow == 1);
    CHECK(cpu.read_token == UINT64_C(0x5ec0de5ec0de5ec0));
    CHECK(cpu.read_incarnation == UINT64_C(0x1111111111111111));
    CHECK(cpu.read_count == UINT64_C(0x2222222222222222));
    CHECK(cpu.read_views[0][0] == UINT64_C(0x3333333333333333));
    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
