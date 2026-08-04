#include "support.h"
#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/guard.h"
#include "../src/arch/aarch64/projection.h"
#include "../src/arch/aarch64/stub.h"
#include "../src/arch/aarch64/trace.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "trace:%d: %s\n", __LINE__, #x); return 1; } } while (0)
#define LDR_X(rt, rn) (0xf9400000u | ((uint32_t)(rn) << 5) | (uint32_t)(rt))
#define STR_X(rt, rn) (0xf9000000u | ((uint32_t)(rn) << 5) | (uint32_t)(rt))

typedef struct executable_memory {
    uint8_t *address;
    uint64_t capacity;
} executable_memory;

typedef struct active_view_observer {
    hl_native_aarch64_cpu *cpu;
    uint64_t incarnation;
    uint64_t authority;
    uint64_t publications;
} active_view_observer;

static uint32_t observe_active_view(void *opaque, const hl_native_fault_scope *scope) {
    active_view_observer *observer = opaque;
    (void)scope;
    observer->incarnation = observer->cpu->active_view_incarnation;
    observer->authority = observer->cpu->active_view_authority;
    observer->publications++;
    return HL_NATIVE_OK;
}

static void release_active_view(void *opaque, const hl_native_fault_scope *scope) {
    (void)opaque;
    (void)scope;
}

static uint32_t reject_active_view(void *opaque, const hl_native_fault_scope *scope) {
    active_view_observer *observer = opaque;
    (void)scope;
    observer->incarnation = observer->cpu->active_view_incarnation;
    observer->authority = observer->cpu->active_view_authority;
    observer->publications++;
    return HL_NATIVE_STATE;
}

static hl_native_status executable_reserve(void *opaque, uint64_t capacity, uint64_t alignment,
                                           uint32_t dual, hl_native_mapping *output) {
    executable_memory *memory = opaque;
    void *address;
    (void)alignment;
    if (dual != 0) return HL_NATIVE_PLATFORM;
    address = mmap(NULL, (size_t)capacity, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (address == MAP_FAILED) return HL_NATIVE_MEMORY;
    memory->address = address;
    memory->capacity = capacity;
    *output = (hl_native_mapping){.abi = HL_NATIVE_ABI, .size = sizeof(*output), .handle = 1,
                                  .writable = (uint64_t)(uintptr_t)address,
                                  .executable = (uint64_t)(uintptr_t)address, .capacity = capacity};
    return HL_NATIVE_OK;
}

static hl_native_status executable_release(void *opaque, hl_native_handle handle) {
    executable_memory *memory = opaque;
    if (handle != 1 || memory->address == NULL) return HL_NATIVE_ARGUMENT;
    if (munmap(memory->address, (size_t)memory->capacity) != 0) return HL_NATIVE_PLATFORM;
    memory->address = NULL;
    return HL_NATIVE_OK;
}

static hl_native_status executable_publish(void *opaque, hl_native_handle handle,
                                           uint64_t offset, uint64_t size) {
    executable_memory *memory = opaque;
    if (handle != 1 || offset > memory->capacity || size > memory->capacity - offset)
        return HL_NATIVE_ARGUMENT;
    return HL_NATIVE_OK;
}

static hl_native_status executable_repair(void *opaque, hl_native_mapping *mapping, uint32_t preserve) {
    executable_memory *memory = opaque;
    (void)preserve;
    if (mapping->handle != 1 || mapping->writable != (uint64_t)(uintptr_t)memory->address)
        return HL_NATIVE_ARGUMENT;
    return HL_NATIVE_OK;
}

static hl_native_status executable_begin(void *opaque) {
    executable_memory *memory = opaque;
    return mprotect(memory->address, (size_t)memory->capacity, PROT_READ | PROT_WRITE) == 0
               ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static hl_native_status executable_end(void *opaque) {
    executable_memory *memory = opaque;
    __builtin___clear_cache((char *)memory->address, (char *)memory->address + memory->capacity);
    return mprotect(memory->address, (size_t)memory->capacity, PROT_READ | PROT_EXEC) == 0
               ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static hl_native_memory executable_services(executable_memory *memory) {
    return (hl_native_memory){.abi = HL_NATIVE_ABI, .size = sizeof(hl_native_memory), .context = memory,
                              .reserve = executable_reserve, .release = executable_release,
                              .publish = executable_publish, .repair = executable_repair,
                              .write_begin = executable_begin, .write_end = executable_end};
}

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    cpu->budget = UINT64_MAX;
    cpu->executed = 0;
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

static void register_sentinels(hl_native_aarch64_cpu *cpu, uint64_t target) {
    for (unsigned index = 0; index < 31; index++)
        cpu->registers[index] = UINT64_C(0x9a00000000000000) | index;
    cpu->registers[30] = target;
}

static int registers_match(const hl_native_aarch64_cpu *cpu, uint64_t target) {
    for (unsigned index = 0; index < 31; index++) {
        uint64_t expected = index == 30 ? target : UINT64_C(0x9a00000000000000) | index;
        if (cpu->registers[index] != expected) return 0;
    }
    return 1;
}

typedef struct source_provider {
    uint32_t word;
    uint32_t next;
    uint32_t calls;
    uint32_t stale;
    uint64_t guest;
    size_t size;
} source_provider;

typedef struct operand_provider {
    uint64_t value;
    uint64_t guest;
    uint32_t permissions;
    uint32_t calls;
    uint32_t result;
    uint32_t stale;
    uint64_t expected_epoch;
} operand_provider;

#define OPERAND_VIEW_COUNT 6
typedef struct operand_views {
    uint64_t values[OPERAND_VIEW_COUNT][2];
    uint64_t guests[OPERAND_VIEW_COUNT];
    uint64_t lengths[OPERAND_VIEW_COUNT];
    uint32_t permissions[OPERAND_VIEW_COUNT];
    uint32_t calls;
} operand_views;

static uint32_t resolve_views(void *opaque, uint64_t address, uint64_t size,
                              uint32_t access, uint64_t mapping, uint64_t epoch,
                              hl_native_projection_view *output) {
    operand_views *provider = opaque;
    provider->calls++;
    if (mapping != 7 || epoch != 8 || size == 0 || address > UINT64_MAX - size)
        return HL_NATIVE_OPERAND_EPOCH;
    for (size_t index = 0; index < OPERAND_VIEW_COUNT; index++) {
        uint64_t first = provider->guests[index];
        uint64_t length = provider->lengths[index];
        if (length == 0 || first > UINT64_MAX - length || address < first ||
            address + size > first + length)
            continue;
        if ((provider->permissions[index] & access) != access)
            return HL_NATIVE_OPERAND_FAULT;
        *output = (hl_native_projection_view){first, first + length,
            (uint64_t)(uintptr_t)&provider->values[index][0], mapping,
            provider->permissions[index], 0};
        return HL_NATIVE_OPERAND_RESOLVED;
    }
    return HL_NATIVE_OPERAND_FAULT;
}

static uint32_t resolve_operand(void *opaque, uint64_t address, uint64_t size,
                                uint32_t access, uint64_t mapping, uint64_t epoch,
                                hl_native_projection_view *output) {
    operand_provider *provider = opaque;
    provider->calls++;
    uint64_t expected_epoch = provider->expected_epoch ? provider->expected_epoch : 8;
    if (provider->stale || mapping != 7 || epoch != expected_epoch) return HL_NATIVE_OPERAND_EPOCH;
    if (provider->result != HL_NATIVE_OPERAND_RESOLVED) return provider->result;
    if (address != provider->guest || size != 8 || (provider->permissions & access) != access)
        return HL_NATIVE_OPERAND_FAULT;
    *output = (hl_native_projection_view){address, address + size,
        (uint64_t)(uintptr_t)&provider->value, mapping, provider->permissions, 0};
    return HL_NATIVE_OPERAND_RESOLVED;
}

static int resolve_source(void *opaque, uint64_t pc, uint64_t mapping,
                          uint64_t epoch, hl_native_source_span *output) {
    source_provider *provider = opaque;
    provider->calls++;
    if (pc != provider->guest || mapping != 7 || epoch != 8) return 0;
    size_t size = provider->size ? provider->size : sizeof(provider->word);
    *output = (hl_native_source_span){pc, (const uint8_t *)&provider->word,
                                     size, mapping, epoch + provider->stale};
    return 1;
}

static int effect_analysis(void) {
    struct effect_case {
        uint32_t word;
        uint32_t definitions;
        uint8_t terminal;
        uint8_t control;
    } cases[] = {
        {0x91000405u, UINT32_C(1) << 5, 0, 0}, /* add x5,x0,#1 */
        {0x910043ffu, UINT32_C(1) << 31, 0, 0}, /* add sp,sp,#16 */
        {0xb100041fu, 0, 0, 0}, /* adds xzr,x0,#1 */
        {0x8b21401fu, UINT32_C(1) << 31, 0, 0}, /* add sp,x0,w1,uxtw */
        {0xcb21401fu, UINT32_C(1) << 31, 0, 0}, /* sub sp,x0,w1,uxtw */
        {0xab21401fu, 0, 0, 0}, /* adds xzr,x0,w1,uxtw */
        {0xeb21401fu, 0, 0, 0}, /* subs xzr,x0,w1,uxtw */
        {0xf94003e9u, UINT32_C(1) << 9, 0, 0}, /* ldr x9,[sp] */
        {0xf90003e1u, 0, 0, 0}, /* str x1,[sp] */
        {0xf81f0fe1u, UINT32_C(1) << 31, 0, 0}, /* str x1,[sp,#-16]! */
        {0xa8c17bfdu, (UINT32_C(1) << 29) | (UINT32_C(1) << 30) |
                          (UINT32_C(1) << 31), 0, 0}, /* ldp x29,x30,[sp],#16 */
        {0x14000000u, 0, 1, 1}, /* b . */
        {0x94000000u, UINT32_C(1) << 30, 1, 1}, /* bl . */
        {0xd65f03c0u, 0, 1, 1}, /* ret */
        {0xd4000001u, UINT32_MAX, 1, 0}, /* svc */
        {0x00000000u, UINT32_MAX, 1, 0}, /* unallocated */
    };
    for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
        hl_a64_instruction_effect effect = hl_a64_trace_effect(cases[index].word, 0x1000);
        CHECK(effect.gpr_definitions == cases[index].definitions);
        CHECK(effect.terminal == cases[index].terminal);
        CHECK(effect.control == cases[index].control);
    }
    return 0;
}

static int range_planning(void) {
    const uint32_t xn[] = {
        0xf85f8140u, /* ldur x0,[x10,#-8] */
        0x91000463u, /* add x3,x3,#1 */
        0xf9000941u, /* str x1,[x10,#16] */
    };
    const uint32_t sp[] = {0xf94007e0u, 0xf9000be1u}; /* [sp,#8], [sp,#16] */
    hl_a64_range_plan plan;
    CHECK(hl_a64_trace_range_plan(xn, 3, 0x1000, &plan));
    CHECK(plan.base == 10 && plan.minimum == -8 && plan.maximum == 24);
    CHECK(plan.permissions == (HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE));
    CHECK(plan.members == ((UINT32_C(1) << 0) | (UINT32_C(1) << 2)) && plan.access_count == 2);
    CHECK(hl_a64_trace_range_plan(sp, 2, 0x2000, &plan));
    CHECK(plan.base == 31 && plan.minimum == 8 && plan.maximum == 24 && plan.members == 3);

    struct reject_case { const uint32_t *words; size_t count; uint64_t pc; };
    const uint32_t one[] = {0xf9400540u};
    const uint32_t base_definition[] = {0xf940054au, 0xf9000941u}; /* ldr x10,[x10,#8] */
    const uint32_t terminal[] = {0xf9400540u, 0x14000000u, 0xf9000941u};
    const uint32_t unknown[] = {0xf9400540u, 0x00000000u, 0xf9000941u};
    const uint32_t bases[] = {0xf9400540u, 0xf9000961u}; /* x10 then x11 */
    const uint32_t literal[] = {0x58000040u, 0xf9400540u};
    const uint32_t writeback[] = {0xf8408540u, 0xf9000941u};
    const uint32_t register_offset[] = {0xf8616940u, 0xf9000941u};
    const uint32_t pair[] = {0xa9400540u, 0xf9000941u};
    const uint32_t vector[] = {0x3dc00140u, 0xf9000941u};
    const uint32_t exclusive[] = {0xc85f7d40u, 0xf9000941u};
    const uint32_t atomic[] = {0xf8200141u, 0xf9000941u};
    const struct reject_case cases[] = {
        {one, 1, 0x3000}, {base_definition, 2, 0x3000},
        {terminal, 3, 0x3000}, {unknown, 3, 0x3000}, {bases, 2, 0x3000},
        {literal, 2, 0x3000}, {writeback, 2, 0x3000}, {register_offset, 2, 0x3000},
        {pair, 2, 0x3000}, {vector, 2, 0x3000}, {exclusive, 2, 0x3000},
        {atomic, 2, 0x3000},
        {sp, 2, UINT64_MAX - 3},
    };
    for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++)
        CHECK(!hl_a64_trace_range_plan(cases[index].words, cases[index].count,
                                       cases[index].pc, &plan));
    return 0;
}

static int certificate_authentication(void) {
    hl_native_aarch64_cpu valid = {0};
    valid.read_token = 7;
    valid.read_incarnation = 7;
    valid.read_count = 1;
    valid.read_views[0][0] = 0x1000;
    valid.read_views[0][1] = 0x1100;
    valid.read_views[0][2] = 0x4000;
    valid.read_views[0][3] = HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE;
    valid.fault_address = 0xaaaa;
    valid.fault_access = 0xbbbb;
    valid.fault_size = 0xcccc;
    valid.dirty_first = 0x1020;
    valid.dirty_last = 0x1028;
    valid.dirty_count = 3;
    valid.memory_written = 1;
    uint64_t delta = 0;
    hl_native_aarch64_cpu before = valid;
    CHECK(hl_a64_trace_certificate_check(&valid, 0x1040, -16, 24,
          HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE, 7, 91, 91, &delta));
    CHECK(delta == 0x4000 && memcmp(&valid, &before, sizeof(valid)) == 0);

    struct certificate_case {
        hl_native_aarch64_cpu cpu;
        uint64_t base;
        int64_t minimum;
        int64_t maximum;
        uint32_t permissions;
        uint64_t incarnation;
        uint64_t expected;
        uint64_t active;
    } cases[8];
    for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++)
        cases[index] = (struct certificate_case){valid, 0x1040, -16, 24,
            HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE, 7, 91, 91};
    cases[0].cpu.read_token = 0; /* unpublished/stale token */
    cases[1].cpu.read_incarnation = 6; /* stale descriptor incarnation */
    cases[2].cpu.read_views[0][3] = HL_NATIVE_ACCESS_READ; /* mixed R/W denied */
    cases[3].base = 0x1008; cases[3].minimum = -16; /* below descriptor */
    cases[4].base = 0x10f8; cases[4].maximum = 16; /* above descriptor */
    cases[5].base = UINT64_MAX - 3; cases[5].maximum = 8; /* envelope overflow */
    cases[6].incarnation = 8; /* mapping rotation */
    cases[7].active = 92; /* authority identity rotation */
    for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
        hl_native_aarch64_cpu snapshot = cases[index].cpu;
        delta = UINT64_MAX;
        CHECK(!hl_a64_trace_certificate_check(&cases[index].cpu, cases[index].base,
              cases[index].minimum, cases[index].maximum, cases[index].permissions,
              cases[index].incarnation, cases[index].expected, cases[index].active, &delta));
        CHECK(delta == 0 && memcmp(&cases[index].cpu, &snapshot, sizeof(snapshot)) == 0);
    }
    return 0;
}

static int loop_planning(void) {
    const uint32_t hot[] = {
        UINT32_C(0xad008460), UINT32_C(0xad428420),
        UINT32_C(0xad018c62), UINT32_C(0xad438c22),
        UINT32_C(0x91010021), UINT32_C(0x91010063),
        UINT32_C(0xf1010042), UINT32_C(0x54ffff28),
    };
    hl_a64_loop_plan plan;
    CHECK(hl_a64_trace_loop_plan(hot, 8, UINT64_C(0x17a740), &plan));
    CHECK(plan.loop_pc == UINT64_C(0x17a740) && plan.instruction_count == 8 &&
          plan.memory_count == 4 && plan.trip == 2 && plan.decrement == 64 && plan.range_count == 2);
    CHECK(plan.ranges[0].base == 3 && plan.ranges[0].step == 64 &&
          plan.ranges[0].minimum == 16 && plan.ranges[0].maximum == 80 &&
          plan.ranges[0].permissions == HL_NATIVE_ACCESS_WRITE);
    CHECK(plan.ranges[1].base == 1 && plan.ranges[1].step == 64 &&
          plan.ranges[1].minimum == 80 && plan.ranges[1].maximum == 144 &&
          plan.ranges[1].permissions == HL_NATIVE_ACCESS_READ);

    uint32_t changed[8];
    memcpy(changed, hot, sizeof(changed));
    changed[7] = UINT32_C(0x54ffff48); /* side/backedge target is not the loop entry */
    CHECK(!hl_a64_trace_loop_plan(changed, 8, UINT64_C(0x17a740), &plan));
    memcpy(changed, hot, sizeof(changed));
    changed[4] = UINT32_C(0x91010024); /* affine base is clobbered into another register */
    CHECK(!hl_a64_trace_loop_plan(changed, 8, UINT64_C(0x17a740), &plan));
    memcpy(changed, hot, sizeof(changed));
    changed[0] = UINT32_C(0xf9400863); /* scalar load clobbers its affine base x3 */
    CHECK(!hl_a64_trace_loop_plan(changed, 8, UINT64_C(0x17a740), &plan));
    memcpy(changed, hot, sizeof(changed));
    changed[1] = UINT32_C(0xf9402822); /* scalar load clobbers trip x2 */
    CHECK(!hl_a64_trace_loop_plan(changed, 8, UINT64_C(0x17a740), &plan));
    memcpy(changed, hot, sizeof(changed));
    changed[1] = UINT32_C(0xa9451021); /* pair load clobbers affine base x1 */
    CHECK(!hl_a64_trace_loop_plan(changed, 8, UINT64_C(0x17a740), &plan));
    memcpy(changed, hot, sizeof(changed));
    changed[1] = UINT32_C(0xa9451022); /* pair load clobbers trip x2 */
    CHECK(!hl_a64_trace_loop_plan(changed, 8, UINT64_C(0x17a740), &plan));
    memcpy(changed, hot, sizeof(changed));
    changed[6] = UINT32_C(0x91000442); /* trip is not a flag-setting decrement */
    CHECK(!hl_a64_trace_loop_plan(changed, 8, UINT64_C(0x17a740), &plan));
    memcpy(changed, hot, sizeof(changed));
    changed[0] = UINT32_C(0xacc08460); /* vector pair with writeback */
    CHECK(!hl_a64_trace_loop_plan(changed, 8, UINT64_C(0x17a740), &plan));
    memcpy(changed, hot, sizeof(changed));
    changed[0] = UINT32_C(0xf8200060); /* atomic memory operation */
    CHECK(!hl_a64_trace_loop_plan(changed, 8, UINT64_C(0x17a740), &plan));
    memcpy(changed, hot, sizeof(changed));
    changed[0] = UINT32_C(0xc85f7c60); /* exclusive memory operation */
    CHECK(!hl_a64_trace_loop_plan(changed, 8, UINT64_C(0x17a740), &plan));
    memcpy(changed, hot, sizeof(changed));
    changed[2] = UINT32_C(0xad020c62); /* store intervals leave a hole before the next iteration */
    CHECK(!hl_a64_trace_loop_plan(changed, 8, UINT64_C(0x17a740), &plan));
    CHECK(!hl_a64_trace_loop_plan(hot, 8, UINT64_MAX - 16, &plan));
    return 0;
}

static int loop_preflight(void) {
    const uint32_t hot[] = {
        UINT32_C(0xad008460), UINT32_C(0xad428420),
        UINT32_C(0xad018c62), UINT32_C(0xad438c22),
        UINT32_C(0x91010021), UINT32_C(0x91010063),
        UINT32_C(0xf1010042), UINT32_C(0x54ffff28),
    };
    hl_a64_loop_plan plan;
    hl_a64_loop_entry entry;
    hl_native_aarch64_cpu cpu = {0};
    hl_native_aarch64_cpu changed;
    CHECK(hl_a64_trace_loop_plan(hot, 8, UINT64_C(0x17a740), &plan));
    cpu.registers[1] = UINT64_C(0x2000) - 80;
    cpu.registers[2] = 192;
    cpu.registers[3] = UINT64_C(0x3000) - 16;
    cpu.active_authority = 91;
    cpu.read_token = 7;
    cpu.read_incarnation = 7;
    cpu.read_count = 2;
    cpu.read_views[0][0] = 0x2000;
    cpu.read_views[0][1] = 0x2100;
    cpu.read_views[0][2] = 0x100000;
    cpu.read_views[0][3] = HL_NATIVE_ACCESS_READ;
    cpu.read_views[1][0] = 0x3000;
    cpu.read_views[1][1] = 0x3100;
    cpu.read_views[1][2] = 0x200000;
    cpu.read_views[1][3] = HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE;
    cpu.dirty_first = UINT64_MAX;
    CHECK(hl_a64_trace_loop_preflight(&cpu, &plan, 7, 91, 40, &entry) ==
          HL_A64_LOOP_PREFLIGHT_READY);
    CHECK(entry.view_count == 2 && entry.trip == 192 && entry.decrement == 64 &&
          entry.iterations == 3 && entry.instruction_count == 8 &&
          entry.budget_iterations == 3 && entry.mapping_incarnation == 7 && entry.authority == 91 &&
          entry.views[0][0] == 0x3000 && entry.views[0][1] == 0x30c0 &&
          entry.views[0][2] == 0x3000 && entry.views[0][3] == 0x3100 &&
          entry.views[0][4] == 0x200000 && entry.views[0][5] == 3 &&
          entry.views[1][0] == 0x2000 && entry.views[1][1] == 0x20c0 &&
          entry.views[1][2] == 0x2000 && entry.views[1][3] == 0x2100 && entry.executable == 0);

    changed = cpu;
    changed.registers[1] = UINT64_C(0x3200) - 80;
    changed.read_count = 1;
    changed.read_views[0][0] = 0x3000;
    changed.read_views[0][1] = 0x3400;
    changed.read_views[0][2] = 0x400000;
    changed.read_views[0][3] = HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE;
    CHECK(hl_a64_trace_loop_preflight(&changed, &plan, 7, 91, 40, &entry) ==
          HL_A64_LOOP_PREFLIGHT_READY);
    CHECK(entry.views[0][0] == 0x3000 && entry.views[1][0] == 0x3200 &&
          entry.views[0][2] == 0x3000 && entry.views[0][3] == 0x3400 &&
          entry.views[1][2] == 0x3000 && entry.views[1][3] == 0x3400);

    changed = cpu;
    changed.active_authority = 92;
    CHECK(hl_a64_trace_loop_preflight(&changed, &plan, 7, 91, 40, &entry) == 0);
    changed = cpu;
    changed.read_token = 8;
    CHECK(hl_a64_trace_loop_preflight(&changed, &plan, 7, 91, 40, &entry) == 0);
    changed = cpu;
    changed.read_incarnation = 8;
    CHECK(hl_a64_trace_loop_preflight(&changed, &plan, 7, 91, 40, &entry) == 0);
    changed = cpu;
    changed.registers[3] = UINT64_MAX - 32;
    CHECK(hl_a64_trace_loop_preflight(&changed, &plan, 7, 91, 40, &entry) == 0);
    changed = cpu;
    changed.registers[2] = UINT64_MAX;
    plan.instruction_count = UINT32_MAX;
    CHECK(hl_a64_trace_loop_preflight(&changed, &plan, 7, 91, UINT64_MAX, &entry) == 0);
    CHECK(hl_a64_trace_loop_plan(hot, 8, UINT64_C(0x17a740), &plan));
    changed = cpu;
    changed.read_views[1][3] = HL_NATIVE_ACCESS_READ;
    CHECK(hl_a64_trace_loop_preflight(&changed, &plan, 7, 91, 40, &entry) == 0);
    changed = cpu;
    changed.dirty_first = 0x4000;
    changed.dirty_last = 0x4010;
    changed.dirty_count = 16;
    changed.memory_first = 0x4000;
    changed.memory_last = 0x4100;
    CHECK(hl_a64_trace_loop_preflight(&changed, &plan, 7, 91, 40, &entry) ==
          HL_A64_LOOP_PREFLIGHT_EPOCH);
    changed = cpu;
    changed.read_views[1][3] |= 4;
    CHECK(hl_a64_trace_loop_preflight(&changed, &plan, 7, 91, 40, &entry) ==
          HL_A64_LOOP_PREFLIGHT_EPOCH);
    CHECK(entry.executable == 1);
    CHECK(hl_a64_trace_loop_preflight(&cpu, &plan, 7, 91, 7, &entry) == 0);
    return 0;
}

static int guard_modes(void) {
    uint8_t legacy_bytes[4096] = {0}, explicit_bytes[4096] = {0}, authenticated_bytes[4096] = {0};
    uint8_t read_bytes[4096] = {0};
    hl_a64_assembler legacy, explicit_legacy, authenticated, read;
    hl_a64_guard legacy_guard = {.pc = 0x4400}, explicit_guard = {.pc = 0x4400};
    hl_a64_guard authenticated_guard = {.pc = 0x4400};
    CHECK(hl_a64_assembler_begin(&legacy, legacy_bytes, legacy_bytes, sizeof(legacy_bytes)));
    CHECK(hl_a64_assembler_begin(&explicit_legacy, explicit_bytes, explicit_bytes, sizeof(explicit_bytes)));
    CHECK(hl_a64_assembler_begin(&authenticated, authenticated_bytes, authenticated_bytes,
                                 sizeof(authenticated_bytes)));
    CHECK(hl_a64_assembler_begin(&read, read_bytes, read_bytes, sizeof(read_bytes)));
    hl_a64_guard_begin(&legacy, 8, HL_A64_PERMISSION_WRITE, &legacy_guard);
    hl_a64_guard_begin_mode(&explicit_legacy, 8, HL_A64_PERMISSION_WRITE,
                            HL_A64_GUARD_LEGACY, &explicit_guard);
    hl_a64_guard_begin_mode(&authenticated, 8, HL_A64_PERMISSION_WRITE,
                            HL_A64_GUARD_AUTHENTICATED_MEMBER, &authenticated_guard);
    size_t legacy_size = hl_a64_assembler_size(&legacy);
    size_t authenticated_size = hl_a64_assembler_size(&authenticated);
    CHECK(legacy_size == hl_a64_assembler_size(&explicit_legacy));
    CHECK(memcmp(legacy_bytes, explicit_bytes, legacy_size) == 0);
    /* Invalid certificate (zero) falls through the first two words into the
     * byte-identical legacy guard.  The remaining nine words are one branch
     * around the dormant valid path and its eight-word delta application. */
    CHECK((authenticated_size - legacy_size) / sizeof(uint32_t) == 11);
    CHECK((((uint32_t *)authenticated_bytes)[1] & UINT32_C(0xff00001f)) ==
          UINT32_C(0xb5000011));
    CHECK(memcmp(authenticated_bytes + 2 * sizeof(uint32_t), legacy_bytes, legacy_size) == 0);
    CHECK(authenticated_guard.below == (uint32_t *)(authenticated_bytes +
          2 * sizeof(uint32_t) + ((uint8_t *)legacy_guard.below - legacy_bytes)));
    CHECK(authenticated_guard.resume == authenticated_bytes +
          2 * sizeof(uint32_t) + (legacy_guard.resume - legacy_bytes));
    {
        hl_a64_guard read_guard = {.pc = 0x4400};
        size_t delta_word = SIZE_MAX, count_bound_word = SIZE_MAX;
        hl_a64_guard_begin(&read, 8, HL_A64_PERMISSION_READ, &read_guard);
        size_t words = hl_a64_assembler_size(&read) / sizeof(uint32_t);
        for (size_t index = 0; index < words; ++index) {
            uint32_t word = ((uint32_t *)read_bytes)[index];
            if (delta_word == SIZE_MAX && word == UINT32_C(0x8b110210)) delta_word = index;
            if (count_bound_word == SIZE_MAX && word == UINT32_C(0xf100123f)) count_bound_word = index;
        }
        /* Slot zero restores flags/scratch and branches after its delta add;
         * only then do the two deferred load/count-bound words execute. */
        CHECK(delta_word != SIZE_MAX && count_bound_word == delta_word + 6);
    }
    return 0;
}

static int static_density_accounting(void) {
    const uint32_t words[] = {
        UINT32_C(0xad428420), /* ldp q0,q1,[x1,#80] */
        UINT32_C(0xad008460), /* stp q0,q1,[x3,#16] */
        UINT32_C(0xf9400044), /* ldr x4,[x2] */
        UINT32_C(0x91000484), /* add x4,x4,#1 */
        UINT32_C(0xd4000001), /* svc #0 */
    };
    const hl_a64_source_span span = {0x4800, (const uint8_t *)words, sizeof(words), 7, 11};
    const hl_a64_source source = {&span, 1, 7, 11};
    uint8_t plain[HL_A64_TRACE_MAX_BYTES] = {0};
    uint8_t accounted[HL_A64_TRACE_MAX_BYTES] = {0};
    hl_a64_trace_result plain_result, accounted_result, failed_result;
    hl_a64_trace_density density, failed_density;
    CHECK(hl_a64_trace_build(&source, 0x4800, 5, plain, sizeof(plain), &plain_result));
    CHECK(hl_a64_trace_build_density(&source, 0x4800, 5, accounted, sizeof(accounted),
                                     &accounted_result, &density));
    CHECK(plain_result.code_size == accounted_result.code_size);
    CHECK(memcmp(plain, accounted, plain_result.code_size) == 0);
    CHECK(density.families[HL_A64_DENSITY_PAIR_READ].guest_instructions == 1);
    CHECK(density.families[HL_A64_DENSITY_PAIR_WRITE].guest_instructions == 1);
    CHECK(density.families[HL_A64_DENSITY_SCALAR_MEMORY].guest_instructions == 1);
    CHECK(density.families[HL_A64_DENSITY_CONTROL].guest_instructions == 1);
    CHECK(density.families[HL_A64_DENSITY_OTHER].guest_instructions == 1);
    uint64_t instructions = 0, words_total = density.overhead_words;
    for (size_t family = 0; family < HL_A64_DENSITY_FAMILY_COUNT; family++) {
        instructions += density.families[family].guest_instructions;
        words_total += density.families[family].hot_words + density.families[family].cold_words;
    }
    CHECK(instructions == accounted_result.instruction_count);
    CHECK(words_total == density.total_words);
    CHECK(density.total_words * sizeof(uint32_t) == accounted_result.code_size);
    CHECK(density.families[HL_A64_DENSITY_PAIR_READ].cold_words != 0);
    CHECK(density.families[HL_A64_DENSITY_PAIR_WRITE].cold_words != 0);
    CHECK(density.families[HL_A64_DENSITY_SCALAR_MEMORY].cold_words != 0);
    CHECK(density.saturated == 0);

    memset(&failed_density, 0xa5, sizeof(failed_density));
    memset(&failed_result, 0xa5, sizeof(failed_result));
    CHECK(!hl_a64_trace_build_density(&source, 0x4800, 5, accounted, 1,
                                      &failed_result, &failed_density));
    CHECK(memcmp(&failed_density, &(hl_a64_trace_density){0}, sizeof(failed_density)) == 0);
    return 0;
}

int main(void) {
    if (effect_analysis() != 0) return 1;
    if (loop_preflight() != 0) return 1;
    if (range_planning() != 0) return 1;
    if (loop_planning() != 0) return 1;
    if (certificate_authentication() != 0) return 1;
    if (guard_modes() != 0) return 1;
    if (static_density_accounting() != 0) return 1;
#if !defined(__aarch64__)
    return 0;
#else
    const uint32_t words[] = {
        0xd2800061u, /* movz x1,#3 */
        0x91001021u, /* add x1,x1,#4 */
        0xf90003e1u, /* str x1,[sp] */
        0xf94003e9u, /* ldr x9,[sp] */
        0xb27d0021u, /* orr x1,x1,#8 */
        0x9a820023u, /* csel x3,x1,x2,eq */
        0x10000004u, /* adr x4,pc */
        0x9b017c25u, /* mul x5,x1,x1 */
        0x9ac108a6u, /* udiv x6,x5,x1 */
        0x9ac12027u, /* lslv x7,x1,x1 */
        0xd3401ce8u, /* ubfm x8,x7,#0,#7 */
        0xfa41e020u, /* ccmp x1,x1,#0,al */
        0xa9bf7bfdu, /* stp x29,x30,[sp,#-16]! */
        0xa8c17bfdu, /* ldp x29,x30,[sp],#16 */
        0x4c007140u, /* st1 {v0.16b},[x10] */
        0x4c407140u, /* ld1 {v0.16b},[x10] */
        0xd4000001u, /* svc #0 */
    };
    hl_a64_source_span span = {0x2000, (const uint8_t *)words, sizeof(words), 3, 4};
    hl_a64_source source = {&span, 1, 3, 4};
    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 4;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    hl_a64_trace_result trace;
    CHECK(code != MAP_FAILED);
    {
        const uint32_t simd_words[] = {
            UINT32_C(0x4e201c20), /* and v0.16b,v1.16b,v0.16b */
            UINT32_C(0x4ebc1f9f), /* mov v31.16b,v28.16b */
            UINT32_C(0x4e3ecfbf), /* fmla v31.4s,v29.4s,v30.4s */
            UINT32_C(0x4ee2cc20), /* fmls v0.2d,v1.2d,v2.2d */
            UINT32_C(0x4e21dbf9), /* scvtf v25.4s,v31.4s */
            UINT32_C(0x6e61d820), /* ucvtf v0.2d,v1.2d */
            UINT32_C(0xd4000001), /* svc */
        };
        const hl_a64_source_span simd_span = {
            0x1f00, (const uint8_t *)simd_words, sizeof(simd_words), 3, 4};
        const hl_a64_source simd_source = {&simd_span, 1, 3, 4};
        CHECK(hl_a64_trace_build(&simd_source, 0x1f00, 7, code, capacity, &trace));
        CHECK(trace.instruction_count == 7 && trace.source_last == 0x1f1c &&
              trace.terminal == HL_NATIVE_EXIT_SYSCALL);
        const uint32_t invalid_words[] = {
            UINT32_C(0x6e3ecfbf), /* unallocated U=1 FMLA neighbor */
            UINT32_C(0x6ee2cc20), /* unallocated U=1 FMLS neighbor */
            UINT32_C(0x0e62cc20), /* reserved one-lane double FMLA */
        };
        for (size_t index = 0; index < sizeof(invalid_words) / sizeof(invalid_words[0]); ++index) {
            const hl_a64_source_span invalid_span = {
                0x1e00, (const uint8_t *)&invalid_words[index], sizeof(invalid_words[index]), 3, 4};
            const hl_a64_source invalid_source = {&invalid_span, 1, 3, 4};
            CHECK(!hl_a64_trace_build(&invalid_source, 0x1e00, 1, code, capacity, &trace));
        }
    }
    {
        const uint32_t literal_word = UINT32_C(0x58000800); /* ldr x0,0x2100 */
        const hl_a64_source_span literal_span = {
            0x2000, (const uint8_t *)&literal_word, sizeof(literal_word), 3, 4};
        const hl_a64_source literal_source = {&literal_span, 1, 3, 4};
        hl_a64_trace_result guarded, direct;
        hl_native_direct_authority authority = {
            .abi = HL_NATIVE_ABI, .size = sizeof(authority),
            .permissions = HL_NATIVE_ACCESS_READ,
            .guest_first = 0x2100, .guest_last = 0x2108, .host_first = 0x100000,
            .mapping_incarnation = 3,
        };
        CHECK(hl_a64_trace_build(&literal_source, 0x2000, 1, code, capacity, &guarded));
        CHECK(!hl_a64_trace_build_direct(&literal_source, 0x2000, 1, code, capacity,
                                         &authority, 0, &direct));
        CHECK(hl_a64_trace_build_direct(&literal_source, 0x2000, 1, code, capacity, &authority, 11, &direct));
        CHECK(direct.code_size < guarded.code_size);
        CHECK(direct.provenance_count == 1 && direct.provenance[0].guest == 0x2000 &&
              direct.provenance[0].access == HL_NATIVE_ACCESS_READ && direct.provenance[0].width == 8);
        authority.guest_last = 0x2107;
        CHECK(hl_a64_trace_build_direct(&literal_source, 0x2000, 1, code, capacity, &authority, 11, &direct));
        CHECK(direct.code_size == guarded.code_size);
        authority.guest_first = 0x20ff;
        authority.guest_last = 0x2108;
        authority.permissions = HL_NATIVE_ACCESS_WRITE;
        CHECK(hl_a64_trace_build_direct(&literal_source, 0x2000, 1, code, capacity, &authority, 11, &direct));
        CHECK(direct.code_size == guarded.code_size);
    }
    CHECK(hl_a64_trace_build(&source, 0x2000, 17, code, capacity, &trace));
    CHECK(trace.code_size != 0 && trace.provenance_count == 23);
    CHECK(trace.body_offset != 0 && trace.body_offset < trace.code_size && trace.instruction_count == 17);
    CHECK(trace.source_first == 0x2000 && trace.source_last == 0x2044);
    for (unsigned i = 0; i < 17; i++) CHECK(trace.provenance[i].guest == 0x2000 + i * 4);
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);
    {
        const uint32_t slot_words[] = {UINT32_C(0xf9400020), UINT32_C(0xd4000001)};
        const hl_a64_source_span slot_span = {
            0x1d00, (const uint8_t *)slot_words, sizeof(slot_words), 3, 4};
        const hl_a64_source slot_source = {&slot_span, 1, 3, 4};
        uint64_t value = UINT64_C(0x1122334455667788);
        CHECK(mprotect(code, capacity, PROT_READ | PROT_WRITE) == 0);
        CHECK(hl_a64_trace_build(&slot_source, 0x1d00, 2, code, capacity, &trace));
        __builtin___clear_cache((char *)code, (char *)code + capacity);
        CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);
        hl_native_aarch64_cpu slot_cpu = {0};
        slot_cpu.program = 0x1d00;
        slot_cpu.registers[1] = 0x1000;
        slot_cpu.memory_first = 1; /* force the run-view selector */
        slot_cpu.memory_last = 2;
        slot_cpu.read_views[0][0] = 0x1000;
        slot_cpu.read_views[0][1] = 0x1008;
        slot_cpu.read_views[0][2] = (uint64_t)(uintptr_t)&value - UINT64_C(0x1000);
        slot_cpu.read_views[0][3] = HL_A64_PERMISSION_READ;
        slot_cpu.read_count = 1;
        slot_cpu.read_incarnation = 7;
        slot_cpu.read_token = 7;
        execute(&slot_cpu, code);
        CHECK(slot_cpu.reason == HL_NATIVE_EXIT_SYSCALL && slot_cpu.registers[0] == value);

        memset(&slot_cpu, 0, sizeof(slot_cpu));
        slot_cpu.program = 0x1d00;
        slot_cpu.registers[1] = 0x2000;
        slot_cpu.memory_first = 1;
        slot_cpu.memory_last = 2;
        slot_cpu.read_views[0][0] = 0x1000; /* slot zero misses */
        slot_cpu.read_views[0][1] = 0x1008;
        slot_cpu.read_views[0][3] = HL_A64_PERMISSION_READ;
        slot_cpu.read_views[1][0] = 0x2000; /* would match if count were trusted */
        slot_cpu.read_views[1][1] = 0x2008;
        slot_cpu.read_views[1][2] = (uint64_t)(uintptr_t)&value - UINT64_C(0x2000);
        slot_cpu.read_views[1][3] = HL_A64_PERMISSION_READ;
        slot_cpu.read_count = 5;
        slot_cpu.read_incarnation = 7;
        slot_cpu.read_token = 7;
        execute(&slot_cpu, code);
        CHECK(slot_cpu.reason == HL_NATIVE_EXIT_FALLBACK && slot_cpu.program == 0x1d00);
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_WRITE) == 0);
    CHECK(hl_a64_trace_build(&source, 0x2000, 17, code, capacity, &trace));
    __builtin___clear_cache((char *)code, (char *)code + capacity);
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);
    hl_native_aarch64_cpu cpu = {0};
    _Alignas(16) uint64_t stack[8] = {0};
    cpu.stack = (uint64_t)(uintptr_t)(stack + 4);
    cpu.memory_first = (uint64_t)(uintptr_t)stack;
    cpu.memory_last = (uint64_t)(uintptr_t)(stack + 8);
    cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    cpu.flags = UINT64_C(0x40000000);
    cpu.registers[2] = 99;
    cpu.registers[10] = (uint64_t)(uintptr_t)stack;
    cpu.registers[29] = UINT64_C(0x2929292929292929);
    cpu.registers[30] = UINT64_C(0x3030303030303030);
    cpu.vectors[0] = UINT64_C(0x0102030405060708);
    cpu.vectors[1] = UINT64_C(0x1112131415161718);
    execute(&cpu, code);
    CHECK(cpu.registers[1] == 15 && cpu.registers[3] == 15 && cpu.registers[9] == 7 && stack[4] == 7);
    CHECK(cpu.registers[4] == 0x2018 && cpu.registers[5] == 225 && cpu.registers[6] == 15);
    CHECK(cpu.registers[7] == 491520 && cpu.registers[8] == 0);
    CHECK(cpu.flags == UINT64_C(0x60000000));
    CHECK(cpu.registers[29] == UINT64_C(0x2929292929292929));
    CHECK(cpu.registers[30] == UINT64_C(0x3030303030303030));
    CHECK(stack[0] == UINT64_C(0x0102030405060708));
    CHECK(stack[1] == UINT64_C(0x1112131415161718));
    CHECK(cpu.reason == HL_NATIVE_EXIT_SYSCALL && cpu.program == 0x2040);
    const uint32_t hint_words[] = {0xd503233fu, 0xd50323bfu, 0xd503245fu, 0xd4000001u};
    const hl_a64_source_span hint_span = {
        0x2100, (const uint8_t *)hint_words, sizeof(hint_words), 3, 4};
    const hl_a64_source hint_source = {&hint_span, 1, 3, 4};
    CHECK(mprotect(code, capacity, PROT_READ | PROT_WRITE) == 0);
    CHECK(hl_a64_trace_build(&hint_source, 0x2100, 4, code, capacity, &trace));
    CHECK(trace.source_last == 0x2110 && trace.terminal == HL_NATIVE_EXIT_SYSCALL);
    __builtin___clear_cache((char *)code, (char *)code + capacity);
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);
    memset(&cpu, 0, sizeof(cpu));
    cpu.registers[30] = UINT64_C(0x3030303030303030);
    execute(&cpu, code);
    CHECK(cpu.registers[30] == UINT64_C(0x3030303030303030));
    CHECK(cpu.reason == HL_NATIVE_EXIT_SYSCALL && cpu.program == 0x210c);
    uint32_t guarded_words[HL_A64_TRACE_MAX_WORDS];
    for (size_t index = 0; index < sizeof(guarded_words) / sizeof(guarded_words[0]); index++)
        guarded_words[index] = 0xf94003e0u; /* ldr x0,[sp] */
    const hl_a64_source_span guarded_span = {
        0x2140, (const uint8_t *)guarded_words, sizeof(guarded_words), 3, 4};
    const hl_a64_source guarded_source = {&guarded_span, 1, 3, 4};
    CHECK(mprotect(code, capacity, PROT_READ | PROT_WRITE) == 0);
    CHECK(!hl_a64_trace_build(&guarded_source, 0x2140, HL_A64_TRACE_MAX_WORDS,
                              code, capacity, &trace));
    CHECK(!hl_a64_trace_build(&guarded_source, 0x2140, 1, code,
                              HL_A64_STUB_MAX_BYTES + 32, &trace));
    uint32_t guarded_write_words[HL_A64_TRACE_MAX_WORDS];
    for (size_t index = 0; index < sizeof(guarded_write_words) / sizeof(guarded_write_words[0]); index++)
        guarded_write_words[index] = 0xf90003e0u; /* str x0,[sp] */
    const hl_a64_source_span guarded_write_span = {
        0x21c0, (const uint8_t *)guarded_write_words, sizeof(guarded_write_words), 3, 4};
    const hl_a64_source guarded_write_source = {&guarded_write_span, 1, 3, 4};
    CHECK(!hl_a64_trace_build(&guarded_write_source, 0x21c0, 1, code,
                              HL_A64_STUB_MAX_BYTES + 32, &trace));
    CHECK(!hl_a64_trace_build(&guarded_write_source, 0x21c0, HL_A64_TRACE_MAX_WORDS,
                              code, capacity, &trace));
    CHECK(!hl_a64_trace_build(&guarded_source, 0x2140, HL_A64_TRACE_MAX_WORDS + 1,
                              code, capacity, &trace));
    const uint32_t shifted_add_words[] = {0x8b0002a0u, 0xd4000001u};
    const hl_a64_source_span shifted_add_span = {
        0x2180, (const uint8_t *)shifted_add_words, sizeof(shifted_add_words), 3, 4};
    const hl_a64_source shifted_add_source = {&shifted_add_span, 1, 3, 4};
    CHECK(mprotect(code, capacity, PROT_READ | PROT_WRITE) == 0);
    CHECK(hl_a64_trace_build(&shifted_add_source, 0x2180, 2, code, capacity, &trace));
    __builtin___clear_cache((char *)code, (char *)code + capacity);
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);
    memset(&cpu, 0, sizeof(cpu));
    cpu.registers[0] = 5;
    cpu.registers[21] = 7;
    execute(&cpu, code);
    CHECK(cpu.registers[0] == 12 && cpu.reason == HL_NATIVE_EXIT_SYSCALL && cpu.program == 0x2184);
    CHECK(mprotect(code, capacity, PROT_READ | PROT_WRITE) == 0);
    CHECK(hl_a64_trace_build(&source, 0x2000, 17, code, capacity, &trace));
    __builtin___clear_cache((char *)code, (char *)code + capacity);
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);
    memset(&cpu, 0, sizeof(cpu));
    stack[4] = UINT64_C(0xaaaaaaaaaaaaaaaa);
    cpu.stack = (uint64_t)(uintptr_t)(stack + 4);
    cpu.memory_first = (uint64_t)(uintptr_t)stack;
    cpu.memory_last = (uint64_t)(uintptr_t)stack + 4;
    cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    execute(&cpu, code);
    CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.program == 0x2008);
    CHECK(stack[4] == UINT64_C(0xaaaaaaaaaaaaaaaa));
    memset(&cpu, 0, sizeof(cpu));
    stack[2] = stack[3] = UINT64_C(0xbbbbbbbbbbbbbbbb);
    cpu.stack = (uint64_t)(uintptr_t)(stack + 4);
    cpu.memory_first = (uint64_t)(uintptr_t)(stack + 3);
    cpu.memory_last = (uint64_t)(uintptr_t)(stack + 8);
    cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    execute(&cpu, code);
    CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.program == 0x2030);
    CHECK(cpu.stack == (uint64_t)(uintptr_t)(stack + 4));
    CHECK(stack[2] == UINT64_C(0xbbbbbbbbbbbbbbbb) && stack[3] == UINT64_C(0xbbbbbbbbbbbbbbbb));
    memset(&cpu, 0, sizeof(cpu));
    stack[7] = UINT64_C(0xcccccccccccccccc);
    cpu.stack = (uint64_t)(uintptr_t)(stack + 4);
    cpu.memory_first = (uint64_t)(uintptr_t)stack;
    cpu.memory_last = (uint64_t)(uintptr_t)(stack + 8);
    cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    cpu.registers[10] = (uint64_t)(uintptr_t)(stack + 7);
    cpu.vectors[0] = UINT64_C(0x0102030405060708);
    cpu.vectors[1] = UINT64_C(0x1112131415161718);
    execute(&cpu, code);
    CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.program == 0x2038);
    CHECK(stack[7] == UINT64_C(0xcccccccccccccccc));
    CHECK(munmap(code, capacity) == 0);

    const uint32_t flow_words[] = {
        0xd2800061u, 0x94000003u, /* movz x1,#3; bl 0x4010 */
        0x91000821u, 0x54000060u, /* add x1,#2; b.eq 0x4018 */
        0x91000421u, 0xd65f03c0u, /* add x1,#1; ret */
        0xd4000001u,              /* svc */
    };
    hl_a64_source_span flow_span = {0x4000, (const uint8_t *)flow_words, sizeof(flow_words), 3, 4};
    hl_a64_source flow = {&flow_span, 1, 3, 4};
    uint8_t *flow_code = mmap(NULL, (size_t)page * 4, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    const uint64_t flow_pc[] = {0x4000, 0x4010, 0x4008, 0x4018};
    const size_t flow_count[] = {2, 2, 2, 1};
    CHECK(flow_code != MAP_FAILED);
    for (unsigned i = 0; i < 4; i++)
        CHECK(hl_a64_trace_build(&flow, flow_pc[i], flow_count[i], flow_code + i * page,
                                 (size_t)page, &trace));
    CHECK(mprotect(flow_code, (size_t)page * 4, PROT_READ | PROT_EXEC) == 0);
    memset(&cpu, 0, sizeof(cpu));
    cpu.flags = UINT64_C(0x40000000);
    for (unsigned i = 0; i < 4; i++) execute(&cpu, flow_code + i * page);
    CHECK(cpu.registers[1] == 6 && cpu.registers[30] == 0x4008);
    CHECK(cpu.reason == HL_NATIVE_EXIT_SYSCALL && cpu.program == 0x4018);
    CHECK(munmap(flow_code, (size_t)page * 4) == 0);

    const uint32_t family_words[] = {
        0xcb3b434eu, /* sub x14,x26,w27,uxtw */
        0xa907c3f1u, /* stp x17,x16,[sp,#120] */
        0xa947c3f1u, /* ldp x17,x16,[sp,#120] */
        0xd4000001u, /* svc */
    };
    hl_a64_source_span family_span = {0x6000, (const uint8_t *)family_words,
                                      sizeof(family_words), 3, 4};
    hl_a64_source family = {&family_span, 1, 3, 4};
    uint8_t *family_code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(family_code != MAP_FAILED);
    CHECK(hl_a64_trace_build(&family, 0x6000, 4, family_code, capacity, &trace));
    CHECK(trace.source_first == 0x6000 && trace.source_last == 0x6010);
    CHECK(trace.provenance_count >= 4);
    for (unsigned i = 0; i < 4; i++) CHECK(trace.provenance[i].guest == 0x6000 + i * 4);
    CHECK(mprotect(family_code, capacity, PROT_READ | PROT_EXEC) == 0);
    memset(&cpu, 0, sizeof(cpu));
    _Alignas(16) uint64_t family_stack[32] = {0};
    cpu.stack = (uint64_t)(uintptr_t)family_stack;
    cpu.memory_first = (uint64_t)(uintptr_t)family_stack;
    cpu.memory_last = (uint64_t)(uintptr_t)(family_stack + 32);
    cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    cpu.registers[16] = UINT64_C(0x1616161616161616);
    cpu.registers[17] = UINT64_C(0x1717171717171717);
    cpu.registers[26] = 100;
    cpu.registers[27] = UINT64_C(0xffffffff00000003);
    execute(&cpu, family_code);
    CHECK(cpu.registers[14] == 97);
    CHECK(cpu.registers[16] == UINT64_C(0x1616161616161616));
    CHECK(cpu.registers[17] == UINT64_C(0x1717171717171717));
    CHECK(family_stack[15] == UINT64_C(0x1717171717171717));
    CHECK(family_stack[16] == UINT64_C(0x1616161616161616));
    CHECK(cpu.reason == HL_NATIVE_EXIT_SYSCALL && cpu.program == 0x600c);
    CHECK(munmap(family_code, capacity) == 0);

    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    hl_native_config config = test_config(&memory, 0);
    hl_native_executor *executor = NULL;
    hl_native_change replace = {.abi = HL_NATIVE_ABI, .size = sizeof(replace),
                                .kind = HL_NATIVE_REPLACE, .mapping_epoch = 3};
    hl_native_code cached;
    uint32_t hit;
    uint8_t scratch[HL_A64_TRACE_MAX_BYTES];
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);
    CHECK(hl_native_synchronize_epoch(executor, 3, 0, 0, 3) == HL_NATIVE_OK);
    for (unsigned i = 0; i < 4; i++) {
        CHECK(hl_a64_trace_cache(executor, &flow, flow_pc[i], flow_count[i], scratch,
                                 sizeof(scratch), &cached, &hit) == HL_NATIVE_OK);
        CHECK(hit == 0);
        CHECK(cached.body != cached.entry);
        CHECK(hl_a64_trace_cache(executor, &flow, flow_pc[i], flow_count[i], scratch,
                                 sizeof(scratch), &cached, &hit) == HL_NATIVE_OK);
        CHECK(hit == 1);
    }
    /* A shorter budget can replace a previously linked target.  The target's
     * incoming direct and indirect links must be restored before its cache
     * identity is retired. */
    CHECK(hl_a64_trace_cache(executor, &flow, 0x4010, 1, scratch,
                             sizeof(scratch), &cached, &hit) == HL_NATIVE_OK);
    CHECK(hit == 0 && cached.source_first == 0x4010 && cached.source_last == 0x4014);
    CHECK(hl_a64_trace_cache(executor, &source, 0x2000, 17, scratch, sizeof(scratch), &cached, &hit) == HL_NATIVE_OK);
    CHECK(hit == 0);
    CHECK(hl_a64_trace_cache(executor, &source, 0x2000, 17, scratch, sizeof(scratch), &cached, &hit) == HL_NATIVE_OK);
    CHECK(hit == 1);
    CHECK(hl_native_synchronize_epoch(executor, 3, 0, 0, 9) == HL_NATIVE_OK);
    CHECK(hl_a64_trace_cache(executor, &source, 0x2000, 17, scratch, sizeof(scratch), &cached, &hit) == HL_NATIVE_OK);
    CHECK(hit == 0); /* authority rotation cannot reuse the prior translation */
    uint32_t unsupported[] = {0xd2800021u, 0};
    hl_a64_source_span bad_span = {0x3000, (const uint8_t *)unsupported, sizeof(unsupported), 3, 4};
    hl_a64_source bad = {&bad_span, 1, 3, 4};
    hl_native_diagnostics before = {.abi = HL_NATIVE_ABI, .size = sizeof(before)}, after = before;
    CHECK(hl_native_diagnose(executor, &before) == HL_NATIVE_OK);
    CHECK(hl_a64_trace_cache(executor, &bad, 0x3000, 2, scratch, sizeof(scratch), &cached, &hit) == HL_NATIVE_OK);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    CHECK(after.publications == before.publications + 1 && after.live_blocks == before.live_blocks + 1);
    hl_native_destroy(executor);

    const uint32_t unsupported_word = 0;
    const hl_a64_source_span run_spans[] = {
        {0x4000, (const uint8_t *)flow_words, sizeof(flow_words), 7, 8},
        {0x5000, (const uint8_t *)&unsupported_word, sizeof(unsupported_word), 7, 8},
    };
    const hl_a64_source run_source = {run_spans, 2, 7, 8};
    const hl_a64_view run_view = {0x1000, 0x2000, 0x1000, 7, HL_A64_PERMISSION_READ, 0};
    const hl_a64_projection run_projection = {&run_view, 1, 7, 0};
    executable_memory run_host = {0};
    hl_native_memory run_memory = executable_services(&run_host);
    hl_native_config run_config = test_config(&run_memory, 0);
    hl_native_executor *run_executor = NULL;
    hl_native_change run_replace = {.abi = HL_NATIVE_ABI, .size = sizeof(run_replace),
                                    .kind = HL_NATIVE_REPLACE, .mapping_epoch = 7};
    hl_native_aarch64_cpu run_state = {0};
    hl_native_cpu run_cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(run_cpu),
                             .architecture = HL_NATIVE_AARCH64, .state.aarch64 = &run_state};
    hl_native_run_request run_request = {.abi = HL_NATIVE_ABI, .size = sizeof(run_request),
                                         .architecture = HL_NATIVE_AARCH64, .mapping_epoch = 7,
                                         .budget = 16, .source = &run_source, .projection = &run_projection};
    active_view_observer view_observer = {.cpu = &run_state};
    run_request.fault_context = &view_observer;
    run_request.fault_publish = observe_active_view;
    run_request.fault_unpublish = release_active_view;
    hl_native_exit run_output = {.abi = HL_NATIVE_ABI, .size = sizeof(run_output)};
    hl_native_diagnostics cold = {.abi = HL_NATIVE_ABI, .size = sizeof(cold)};
    hl_native_diagnostics warm = cold;
    CHECK(hl_native_create(&run_config, &run_executor) == HL_NATIVE_OK);
    CHECK(hl_native_changed(run_executor, &run_replace, 1) == HL_NATIVE_OK);
    {
        const uint32_t literal_words[] = {UINT32_C(0x58000800), UINT32_C(0xd4000001)};
        const hl_a64_source_span literal_span = {
            0x6000, (const uint8_t *)literal_words, sizeof(literal_words), 7, 8};
        const hl_a64_source literal_source = {&literal_span, 1, 7, 8};
        uint64_t literal_value = UINT64_C(0x1122334455667788);
        const hl_a64_view literal_view = {
            0x6100, 0x6108, (uint64_t)(uintptr_t)&literal_value, 7, HL_A64_PERMISSION_READ, 0};
        const hl_a64_projection literal_projection = {&literal_view, 1, 7, 0};
        hl_native_direct_authority literal_authority = {
            .abi = HL_NATIVE_ABI, .size = sizeof(literal_authority),
            .permissions = HL_NATIVE_ACCESS_READ,
            .guest_first = 0x6100, .guest_last = 0x6108,
            .host_first = (uint64_t)(uintptr_t)&literal_value,
            .mapping_incarnation = 7, .mapping_generation = 1, .instruction_generation = 8,
        };
        hl_native_direct_token *literal_token = NULL;
        CHECK(hl_native_direct_register(run_executor, &literal_authority, &literal_token) == HL_NATIVE_OK);
        run_request.source = &literal_source;
        run_request.projection = &literal_projection;
        run_request.budget = 2;
        run_request.memory_mode = 1;
        run_request.authority_generation = hl_native_direct_generation(run_executor, literal_token);
        run_request.direct_token = literal_token;
        run_request.authority_identity = hl_native_direct_identity(run_executor, literal_token);
        memset(&run_state, 0, sizeof(run_state));
        run_state.program = 0x6000;
        CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
        CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && run_state.registers[0] == literal_value);
        CHECK(view_observer.incarnation == 7 &&
              view_observer.authority == run_request.authority_identity);
        CHECK(run_state.active_view_incarnation == 0 && run_state.active_view_authority == 0);
        hl_native_diagnostics literal_cold = {.abi = HL_NATIVE_ABI, .size = sizeof(literal_cold)};
        hl_native_diagnostics literal_warm = {.abi = HL_NATIVE_ABI, .size = sizeof(literal_warm)};
        CHECK(hl_native_diagnose(run_executor, &literal_cold) == HL_NATIVE_OK);
        uint64_t literal_generation = run_request.authority_generation;
        uint64_t literal_identity = run_request.authority_identity;
        CHECK(hl_native_direct_unregister(run_executor, literal_token) == HL_NATIVE_OK);
        literal_token = NULL;
        CHECK(hl_native_direct_register(run_executor, &literal_authority, &literal_token) == HL_NATIVE_OK);
        run_request.authority_generation = hl_native_direct_generation(run_executor, literal_token);
        run_request.direct_token = literal_token;
        run_request.authority_identity = hl_native_direct_identity(run_executor, literal_token);
        CHECK(run_request.authority_generation != literal_generation);
        CHECK(run_request.authority_identity == literal_identity);
        memset(&run_state, 0, sizeof(run_state));
        run_state.program = 0x6000;
        run_state.active_view_incarnation = UINT64_MAX;
        run_state.active_view_authority = UINT64_MAX;
        CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
        CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && run_state.registers[0] == literal_value);
        CHECK(view_observer.incarnation == 7 &&
              view_observer.authority == run_request.authority_identity);
        CHECK(run_state.active_view_incarnation == 0 && run_state.active_view_authority == 0);
        CHECK(hl_native_diagnose(run_executor, &literal_warm) == HL_NATIVE_OK);
        CHECK(literal_warm.publications == literal_cold.publications);
        CHECK(literal_warm.cache_hits > literal_cold.cache_hits);
        const uint32_t scalar_words[][2] = {
            {UINT32_C(0xf9400020), UINT32_C(0xd4000001)}, /* ldr x0,[x1] */
            {UINT32_C(0xf8400020), UINT32_C(0xd4000001)}, /* ldur x0,[x1] */
            {UINT32_C(0xf8626820), UINT32_C(0xd4000001)}, /* ldr x0,[x1,x2] */
        };
        for (size_t scalar = 0; scalar < 3; ++scalar) {
            const hl_a64_source_span scalar_span = {
                0x6200 + scalar * 0x10, (const uint8_t *)scalar_words[scalar],
                sizeof(scalar_words[scalar]), 7, 8};
            const hl_a64_source scalar_source = {&scalar_span, 1, 7, 8};
            run_request.source = &scalar_source;
            memset(&run_state, 0, sizeof(run_state));
            run_state.program = scalar_span.guest_first;
            run_state.registers[1] = 0x6100;
            CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
            CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && run_state.registers[0] == literal_value);
            memset(&run_state, 0, sizeof(run_state));
            run_state.program = scalar_span.guest_first;
            run_state.registers[1] = 0x6101;
            CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
            CHECK(run_output.kind == HL_NATIVE_EXIT_FALLBACK && run_output.instruction == scalar_span.guest_first);
        }
        CHECK(hl_native_direct_unregister(run_executor, literal_token) == HL_NATIVE_OK);
        hl_native_direct_authority store_authority = literal_authority;
        store_authority.permissions = HL_NATIVE_ACCESS_WRITE;
        const hl_a64_view store_view = {
            0x6100, 0x6108, (uint64_t)(uintptr_t)&literal_value, 7, HL_A64_PERMISSION_WRITE, 0};
        const hl_a64_projection store_projection = {&store_view, 1, 7, 0};
        CHECK(hl_native_direct_register(run_executor, &store_authority, &literal_token) == HL_NATIVE_OK);
        run_request.projection = &store_projection;
        run_request.authority_generation = hl_native_direct_generation(run_executor, literal_token);
        run_request.direct_token = literal_token;
        run_request.authority_identity = hl_native_direct_identity(run_executor, literal_token);
        const uint32_t store_words[][2] = {
            {UINT32_C(0xf9000023), UINT32_C(0xd4000001)}, /* str x3,[x1] */
            {UINT32_C(0xf8000023), UINT32_C(0xd4000001)}, /* stur x3,[x1] */
            {UINT32_C(0xf8226823), UINT32_C(0xd4000001)}, /* str x3,[x1,x2] */
        };
        for (size_t scalar = 0; scalar < 3; ++scalar) {
            const hl_a64_source_span store_span = {
                0x6300 + scalar * 0x10, (const uint8_t *)store_words[scalar],
                sizeof(store_words[scalar]), 7, 8};
            const hl_a64_source store_source = {&store_span, 1, 7, 8};
            run_request.source = &store_source;
            literal_value = 0;
            memset(&run_state, 0, sizeof(run_state));
            run_state.program = store_span.guest_first;
            run_state.registers[1] = 0x6100;
            run_state.registers[3] = UINT64_C(0xaabbccddeeff0011);
            CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
            CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && literal_value == run_state.registers[3]);
            CHECK(run_state.memory_written == 1 && run_state.dirty_first == 0x6100 &&
                  run_state.dirty_last == 0x6108);
            literal_value = 0;
            memset(&run_state, 0, sizeof(run_state));
            run_state.program = store_span.guest_first;
            run_state.registers[1] = 0x6101;
            run_state.registers[3] = UINT64_MAX;
            CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
            CHECK(run_output.kind == HL_NATIVE_EXIT_FALLBACK && literal_value == 0 &&
                  run_state.memory_written == 0);
        }
        CHECK(hl_native_direct_unregister(run_executor, literal_token) == HL_NATIVE_OK);
        CHECK(hl_native_destroy(run_executor) == HL_NATIVE_OK);
        run_executor = NULL;
        CHECK(hl_native_create(&run_config, &run_executor) == HL_NATIVE_OK);
        CHECK(hl_native_changed(run_executor, &run_replace, 1) == HL_NATIVE_OK);
        run_request = (hl_native_run_request){.abi = HL_NATIVE_ABI, .size = sizeof(run_request),
            .architecture = HL_NATIVE_AARCH64, .mapping_epoch = 7, .budget = 16,
            .source = &run_source, .projection = &run_projection};
        run_request.fault_context = &view_observer;
        run_request.fault_publish = observe_active_view;
        run_request.fault_unpublish = release_active_view;
    }
    run_state.program = 0x4000;
    run_state.flags = UINT64_C(0x40000000);
    run_state.certificate_valid = UINT64_MAX;
    run_state.certificate_delta = UINT64_MAX;
    run_state.active_view_incarnation = UINT64_MAX;
    run_state.active_view_authority = UINT64_MAX;
    run_state.loop_valid = UINT64_MAX;
    run_state.loop_view_count = UINT64_MAX;
    memset(run_state.loop_views, 0xff, sizeof(run_state.loop_views));
    run_state.loop_mapping_incarnation = UINT64_MAX;
    run_state.loop_authority = UINT64_MAX;
    run_state.loop_trip = UINT64_MAX;
    run_state.loop_decrement = UINT64_MAX;
    run_state.loop_instruction_count = UINT64_MAX;
    run_state.loop_iterations = UINT64_MAX;
    run_state.loop_budget_iterations = UINT64_MAX;
    run_state.loop_executable = UINT64_MAX;
    run_request.fault_publish = reject_active_view;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_STATE);
    CHECK(view_observer.incarnation == 7 && view_observer.authority == 7);
    CHECK(run_state.active_view_incarnation == 0 && run_state.active_view_authority == 0);
    run_request.fault_publish = observe_active_view;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_state.certificate_valid == 0 && run_state.certificate_delta == 0);
    CHECK(view_observer.publications != 0 && view_observer.incarnation == 7 &&
          view_observer.authority == 7);
    CHECK(run_state.active_view_incarnation == 0 && run_state.active_view_authority == 0);
    CHECK(run_state.loop_valid == 0 && run_state.loop_view_count == 0 &&
          run_state.loop_views[0][0] == 0 && run_state.loop_views[1][5] == 0 &&
          run_state.loop_mapping_incarnation == 0 && run_state.loop_authority == 0 &&
          run_state.loop_trip == 0 && run_state.loop_decrement == 0 &&
          run_state.loop_instruction_count == 0 && run_state.loop_iterations == 0 &&
          run_state.loop_budget_iterations == 0 && run_state.loop_executable == 0);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && run_output.instruction == 0x4018);
    CHECK(run_state.registers[1] == 6 && run_state.registers[30] == 0x4008);
    CHECK(hl_native_diagnose(run_executor, &cold) == HL_NATIVE_OK && cold.publications != 0);
    CHECK(cold.publications == 4);
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0x4000;
    run_state.flags = UINT64_C(0x40000000);
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && run_state.registers[1] == 6);
    CHECK(hl_native_diagnose(run_executor, &warm) == HL_NATIVE_OK);
    CHECK(warm.publications == cold.publications && warm.cache_hits > cold.cache_hits);
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0x4000;
    run_state.flags = UINT64_C(0x40000000);
    run_request.budget = 2;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_YIELD && run_output.instruction == 0x4010);
    CHECK(run_state.registers[1] == 3 && run_state.registers[30] == 0x4008);
    run_state.interrupt = 1;
    run_state.program = 0x4000;
    run_request.budget = 16;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_INTERRUPT && run_output.instruction == 0x4000);
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0x5000;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_FALLBACK && run_output.instruction == 0x5000);
    /* The resolver extends the request without withdrawing the established
     * source/projection prefix from 48-byte callers. */
    run_request.size = offsetof(hl_native_run_request, source_context);
    run_request.budget = 1;
    run_state.program = 0x4018;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && run_output.instruction == 0x4018);
    run_request.size = sizeof(run_request);
    const uint32_t provider_branch = 0x94000400u; /* bl 0x9000 from 0x8000 */
    const hl_a64_source_span provider_span = {
        0x8000, (const uint8_t *)&provider_branch, sizeof(provider_branch), 7, 8};
    const hl_a64_source provider_source = {&provider_span, 1, 7, 8};
    source_provider provider = {.word = 0xd4000001u, .guest = 0x9000};
    run_request.source = &provider_source;
    run_request.source_context = &provider;
    run_request.source_resolve = resolve_source;
    run_request.size = offsetof(hl_native_run_request, operand_context);
    run_request.budget = 2;
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0x8000;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && run_output.instruction == 0x9000);
    CHECK(run_state.registers[30] == 0x8004);
    CHECK(provider.calls == 1);
    run_request.size = sizeof(run_request);
    provider.calls = 0;
    run_state.program = 0x8000;
    run_request.budget = 1;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_YIELD && run_output.instruction == 0x9000);
    CHECK(provider.calls == 0);
    const uint32_t provider_cbz = 0x34008000u; /* cbz w0,0x9100 from 0x8100 */
    const hl_a64_source_span cbz_provider_span = {
        0x8100, (const uint8_t *)&provider_cbz, sizeof(provider_cbz), 7, 8};
    const hl_a64_source cbz_provider_source = {&cbz_provider_span, 1, 7, 8};
    provider.guest = 0x9100;
    run_request.source = &cbz_provider_source;
    run_request.budget = 2;
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0x8100;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && run_output.instruction == 0x9100);
    const uint32_t provider_ret = 0xd65f03c0u;
    const hl_a64_source_span ret_provider_span = {
        0x8200, (const uint8_t *)&provider_ret, sizeof(provider_ret), 7, 8};
    const hl_a64_source ret_provider_source = {&ret_provider_span, 1, 7, 8};
    provider.guest = 0x9200;
    run_request.source = &ret_provider_source;
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0x8200;
    register_sentinels(&run_state, 0x9200);
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && run_output.instruction == 0x9200);
    CHECK(registers_match(&run_state, 0x9200));
    /* The monomorphic site hit must preserve the same complete guest image. */
    run_state.program = 0x8200;
    register_sentinels(&run_state, 0x9200);
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && run_output.instruction == 0x9200);
    CHECK(registers_match(&run_state, 0x9200));
    /* A different return target takes the collision/miss path. */
    provider.guest = 0x9300;
    run_state.program = 0x8200;
    register_sentinels(&run_state, 0x9300);
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && run_output.instruction == 0x9300);
    CHECK(registers_match(&run_state, 0x9300));
    /* Invalidation clears the indirect caches and exercises a fresh cold miss. */
    const hl_native_change ret_invalidate = {
        .abi = HL_NATIVE_ABI, .size = sizeof(ret_invalidate), .kind = HL_NATIVE_INVALIDATE,
        .first = 0x8200, .last = 0x8204, .mapping_epoch = 7,
    };
    CHECK(hl_native_changed(run_executor, &ret_invalidate, 1) == HL_NATIVE_OK);
    provider.guest = 0x9200;
    run_state.program = 0x8200;
    register_sentinels(&run_state, 0x9200);
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && run_output.instruction == 0x9200);
    CHECK(registers_match(&run_state, 0x9200));
    /* A resolver target may have an independently versioned executable span.
     * Its guarded exit must recover the exact chained block rather than the
     * overlapping entry source's token or guest-PC range. */
    const uint32_t versioned_branch = 0x94000440u; /* bl 0x9400 from 0x8300 */
    const hl_native_source_span versioned_entry_span = {
        0x8300, (const uint8_t *)&versioned_branch, sizeof(versioned_branch), 7, 8};
    const hl_native_source versioned_entry = {&versioned_entry_span, 1, 7, 8};
    operand_provider versioned_memory = {
        .value = UINT64_C(0x123456789abcdef0), .guest = 0x3000,
        .permissions = HL_A64_PERMISSION_READ, .result = HL_NATIVE_OPERAND_RESOLVED,
        .expected_epoch = 9};
    provider.word = LDR_X(1, 0);
    provider.next = 0xd4000001u;
    provider.guest = 0x9400;
    provider.size = 2 * sizeof(provider.word);
    provider.stale = 1;
    provider.calls = 0;
    run_request.source = &versioned_entry;
    run_request.source_context = &provider;
    run_request.source_resolve = resolve_source;
    run_request.operand_context = &versioned_memory;
    run_request.operand_resolve = resolve_operand;
    run_request.size = sizeof(run_request);
    run_request.budget = 3;
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0x8300;
    run_state.registers[0] = versioned_memory.guest;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && run_output.instruction == 0x9404);
    CHECK(run_state.registers[1] == versioned_memory.value && provider.calls == 1 && versioned_memory.calls == 1);
    provider.word = 0xd4000001u;
    provider.next = 0;
    provider.size = 0;
    provider.stale = 0;
    provider.calls = 0;
    run_state.program = 0xa000;
    run_request.budget = 2;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_FALLBACK && run_output.instruction == 0xa000);
    CHECK(provider.calls == 0);
    provider.stale = 1;
    provider.calls = 0;
    provider.guest = 0x9000;
    run_request.source = &provider_source;
    run_state.program = 0x8000;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && provider.calls == 0);
    const uint32_t operand_load_words[] = {0xf9400001u, 0xd4000001u};
    const hl_a64_source_span operand_load_span = {
        0xb000, (const uint8_t *)operand_load_words, sizeof(operand_load_words), 7, 8};
    const hl_a64_source operand_load_source = {&operand_load_span, 1, 7, 8};
    operand_provider operand_memory = {.value = UINT64_C(0x1122334455667788), .guest = 0x3000,
                               .permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE,
                               .result = HL_NATIVE_OPERAND_RESOLVED};
    run_request.source = &operand_load_source;
    run_request.operand_context = &operand_memory;
    run_request.operand_resolve = resolve_operand;
    run_request.budget = 2;
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0xb000;
    run_state.registers[0] = operand_memory.guest;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && run_state.registers[1] == operand_memory.value);
    CHECK(operand_memory.calls == 1);
    uint64_t publications = 0;
    hl_native_diagnostics operand_cold = {.abi = HL_NATIVE_ABI, .size = sizeof(operand_cold)};
    CHECK(hl_native_diagnose(run_executor, &operand_cold) == HL_NATIVE_OK);
    publications = operand_cold.publications;
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0xb000;
    run_state.registers[0] = operand_memory.guest;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(operand_memory.calls == 2);
    CHECK(hl_native_diagnose(run_executor, &operand_cold) == HL_NATIVE_OK);
    CHECK(operand_cold.publications == publications);
    const uint32_t operand_store_words[] = {0xf9000001u, 0xd4000001u};
    const hl_a64_source_span operand_store_span = {
        0xb100, (const uint8_t *)operand_store_words, sizeof(operand_store_words), 7, 8};
    const hl_a64_source operand_store_source = {&operand_store_span, 1, 7, 8};
    run_request.source = &operand_store_source;
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0xb100;
    run_state.registers[0] = operand_memory.guest;
    run_state.registers[1] = UINT64_C(0x8877665544332211);
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(operand_memory.value == UINT64_C(0x8877665544332211));
    operand_memory.result = HL_NATIVE_OPERAND_FAULT;
    run_request.source = &operand_load_source;
    run_state.program = 0xb000;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_FAULT && run_output.address == operand_memory.guest &&
          run_output.access == HL_A64_PERMISSION_READ && run_output.instruction == 0xb000);
    operand_memory.result = HL_NATIVE_OPERAND_RESOLVED;
    operand_memory.stale = 1;
    run_state.program = 0xb000;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_EPOCH);
    operand_memory.stale = 0;
    operand_memory.calls = 0;
    run_request.budget = 0;
    run_state.program = 0xb000;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_YIELD && operand_memory.calls == 0);
    run_request.budget = 2;
    run_state.interrupt = 1;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_INTERRUPT && operand_memory.calls == 0);

    const uint32_t alternating_words[] = {
        LDR_X(0, 10), LDR_X(1, 11), LDR_X(2, 12),
        LDR_X(3, 10), LDR_X(4, 11), LDR_X(5, 12), 0xd4000001u,
    };
    const hl_a64_source_span alternating_span = {
        0xb200, (const uint8_t *)alternating_words, sizeof(alternating_words), 7, 8};
    const hl_a64_source alternating_source = {&alternating_span, 1, 7, 8};
    operand_views views = {0};
    for (size_t index = 0; index < OPERAND_VIEW_COUNT; index++) {
        views.values[index][0] = UINT64_C(0x1100000000000000) + index;
        views.guests[index] = UINT64_C(0x200000) + index * UINT64_C(0x200000);
        views.lengths[index] = 16;
        views.permissions[index] = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    }
    run_request.source = &alternating_source;
    run_request.operand_context = &views;
    run_request.operand_resolve = resolve_views;
    run_request.budget = sizeof(alternating_words) / sizeof(alternating_words[0]);
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0xb200;
    run_state.registers[10] = views.guests[0];
    run_state.registers[11] = views.guests[1];
    run_state.registers[12] = views.guests[2];
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && views.calls == 3);
    CHECK(run_state.active_view_incarnation == 0 && run_state.active_view_authority == 0);
    for (size_t index = 0; index < 3; index++) {
        CHECK(run_state.registers[index] == views.values[index][0]);
        CHECK(run_state.registers[index + 3] == views.values[index][0]);
    }
    /* The cache is scoped to one FFI activation: no borrowed host pointer may
     * survive the return to its projection lease owner. */
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0xb200;
    run_state.registers[10] = views.guests[0];
    run_state.registers[11] = views.guests[1];
    run_state.registers[12] = views.guests[2];
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && views.calls == 6);

    const uint32_t eviction_words[] = {
        LDR_X(0, 10), LDR_X(1, 11), LDR_X(2, 12), LDR_X(3, 13),
        LDR_X(4, 14), LDR_X(5, 10), 0xd4000001u,
    };
    const hl_a64_source_span eviction_span = {
        0xb300, (const uint8_t *)eviction_words, sizeof(eviction_words), 7, 8};
    const hl_a64_source eviction_source = {&eviction_span, 1, 7, 8};
    views.calls = 0;
    run_request.source = &eviction_source;
    run_request.budget = sizeof(eviction_words) / sizeof(eviction_words[0]);
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0xb300;
    for (size_t index = 0; index < 5; index++) run_state.registers[10 + index] = views.guests[index];
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && views.calls == 6);

    const uint32_t permission_words[] = {LDR_X(0, 10), STR_X(1, 10), 0xd4000001u};
    const hl_a64_source_span permission_span = {
        0xb400, (const uint8_t *)permission_words, sizeof(permission_words), 7, 8};
    const hl_a64_source permission_source = {&permission_span, 1, 7, 8};
    views.calls = 0;
    views.permissions[0] = HL_A64_PERMISSION_READ;
    uint64_t unchanged = views.values[0][0];
    run_request.source = &permission_source;
    run_request.budget = sizeof(permission_words) / sizeof(permission_words[0]);
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0xb400;
    run_state.registers[10] = views.guests[0];
    run_state.registers[1] = UINT64_C(0xdeadbeefdeadbeef);
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_FAULT && run_output.instruction == 0xb404 &&
          run_output.access == HL_A64_PERMISSION_WRITE && views.calls == 2);
    CHECK(views.values[0][0] == unchanged);

    const uint32_t vector_store_words[] = {UINT32_C(0x3d800143), UINT32_C(0xd4000001)};
    const hl_a64_source_span vector_store_span = {
        0xb480, (const uint8_t *)vector_store_words, sizeof(vector_store_words), 7, 8};
    const hl_a64_source vector_store_source = {&vector_store_span, 1, 7, 8};
    uint64_t vector_unchanged[2] = {views.values[0][0], views.values[0][1]};
    views.calls = 0;
    run_request.source = &vector_store_source;
    run_request.budget = 2;
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0xb480;
    run_state.registers[10] = views.guests[0];
    run_state.vectors[6] = UINT64_C(0x0123456789abcdef);
    run_state.vectors[7] = UINT64_C(0xfedcba9876543210);
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_FAULT && run_output.instruction == 0xb480 &&
          run_output.access == HL_A64_PERMISSION_WRITE && views.calls == 1);
    CHECK(views.values[0][0] == vector_unchanged[0] && views.values[0][1] == vector_unchanged[1]);
    views.permissions[0] = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    views.calls = 0;
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0xb480;
    run_state.registers[10] = views.guests[0];
    run_state.vectors[6] = UINT64_C(0x0123456789abcdef);
    run_state.vectors[7] = UINT64_C(0xfedcba9876543210);
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && views.calls == 1);
    CHECK(views.values[0][0] == run_state.vectors[6] && views.values[0][1] == run_state.vectors[7]);
    CHECK(run_state.memory_written == 1 && run_state.dirty_first == views.guests[0] &&
          run_state.dirty_last == views.guests[0] + 16);

    const uint32_t write_words[] = {STR_X(1, 10), STR_X(2, 10), 0xd4000001u};
    const hl_a64_source_span write_span = {
        0xb500, (const uint8_t *)write_words, sizeof(write_words), 7, 8};
    const hl_a64_source write_source = {&write_span, 1, 7, 8};
    views.calls = 0;
    views.permissions[0] = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    run_request.source = &write_source;
    run_request.budget = sizeof(write_words) / sizeof(write_words[0]);
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0xb500;
    run_state.registers[10] = views.guests[0];
    run_state.registers[1] = UINT64_C(0x1111222233334444);
    run_state.registers[2] = UINT64_C(0x5555666677778888);
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && views.calls == 1);
    CHECK(views.values[0][0] == run_state.registers[2]);

    const uint32_t alternating_write_words[] = {
        STR_X(1, 10), STR_X(2, 11), STR_X(3, 10), STR_X(4, 11), 0xd4000001u,
    };
    const hl_a64_source_span alternating_write_span = {
        0xb580, (const uint8_t *)alternating_write_words,
        sizeof(alternating_write_words), 7, 8};
    const hl_a64_source alternating_write_source = {&alternating_write_span, 1, 7, 8};
    views.calls = 0;
    run_request.source = &alternating_write_source;
    run_request.budget = sizeof(alternating_write_words) / sizeof(alternating_write_words[0]);
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0xb580;
    run_state.registers[10] = views.guests[0];
    run_state.registers[11] = views.guests[1];
    run_state.registers[1] = UINT64_C(0x1111111111111111);
    run_state.registers[2] = UINT64_C(0x2222222222222222);
    run_state.registers[3] = UINT64_C(0x3333333333333333);
    run_state.registers[4] = UINT64_C(0x4444444444444444);
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_SYSCALL && views.calls == 2);
    CHECK(views.values[0][0] == run_state.registers[3]);
    CHECK(views.values[1][0] == run_state.registers[4]);

    const uint32_t boundary_words[] = {LDR_X(0, 10), LDR_X(1, 11), 0xd4000001u};
    const hl_a64_source_span boundary_span = {
        0xb600, (const uint8_t *)boundary_words, sizeof(boundary_words), 7, 8};
    const hl_a64_source boundary_source = {&boundary_span, 1, 7, 8};
    views.calls = 0;
    run_request.source = &boundary_source;
    run_request.budget = sizeof(boundary_words) / sizeof(boundary_words[0]);
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0xb600;
    run_state.registers[10] = views.guests[0];
    run_state.registers[11] = views.guests[0] + 12;
    run_state.registers[1] = UINT64_C(0xabcdefabcdefabcd);
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_FAULT && run_output.instruction == 0xb604 && views.calls == 2);
    CHECK(run_state.registers[1] == UINT64_C(0xabcdefabcdefabcd));

    const uint32_t self_word = 0x14000000u;
    const hl_a64_source_span self_span = {
        0xc000, (const uint8_t *)&self_word, sizeof(self_word), 7, 8};
    const hl_a64_source self_source = {&self_span, 1, 7, 8};
    run_request.source = &self_source;
    run_request.source_resolve = NULL;
    run_request.source_context = NULL;
    run_request.operand_resolve = NULL;
    run_request.operand_context = NULL;
    run_request.budget = 3;
    memset(&run_state, 0, sizeof(run_state));
    run_state.program = 0xc000;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_YIELD && run_state.program == 0xc000 &&
          run_state.executed == 3 && run_state.budget == 0);
    run_request.budget = 3;
    run_state.interrupt = 1;
    CHECK(hl_native_run(run_executor, &run_cpu, &run_request, &run_output) == HL_NATIVE_OK);
    CHECK(run_output.kind == HL_NATIVE_EXIT_INTERRUPT && run_state.executed == 0 && run_state.budget == 3);
    hl_native_destroy(run_executor);
    return 0;
#endif
}
