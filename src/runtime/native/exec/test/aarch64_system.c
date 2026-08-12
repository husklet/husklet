#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/system.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "system:%d: %s\n", __LINE__, #x); return 1; } } while (0)

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
        0xd53bd040u, /* mrs x0,tpidr_el0 */
        0xd53bd05eu, /* mrs x30,tpidr_el0 */
        0xd53bd060u, /* mrs x0,tpidrro_el0 */
        0xd51bd040u, /* msr tpidr_el0,x0 */
        0xd51bd05eu, /* msr tpidr_el0,x30 */
        0xd51bd05fu, /* msr tpidr_el0,xzr */
    };
    size_t offsets[sizeof(words) / sizeof(words[0])];
    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 2;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t index = 0; index < sizeof(words) / sizeof(words[0]); index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_system_emit(&assembler, words[index], 0x6000 + index * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);
    hl_native_aarch64_cpu cpu = {0};
    cpu.tls = UINT64_C(0x123456789abcdef0);
    execute(&cpu, code + offsets[0]); CHECK(cpu.registers[0] == cpu.tls);
    execute(&cpu, code + offsets[1]); CHECK(cpu.registers[30] == cpu.tls);
    cpu.registers[0] = 0;
    execute(&cpu, code + offsets[2]); CHECK(cpu.registers[0] == cpu.tls);
    cpu.registers[0] = UINT64_C(0x0102030405060708);
    execute(&cpu, code + offsets[3]); CHECK(cpu.tls == cpu.registers[0]);
    cpu.registers[30] = UINT64_C(0x8877665544332211);
    execute(&cpu, code + offsets[4]); CHECK(cpu.tls == cpu.registers[30]);
    execute(&cpu, code + offsets[5]); CHECK(cpu.tls == 0);
    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
