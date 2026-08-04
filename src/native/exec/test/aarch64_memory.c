#include "../src/arch/aarch64/memory.h"

#include <stdio.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "memory:%d: %s\n", __LINE__, #x); return 1; } } while (0)

int main(void) {
    hl_a64_memory memory;
    CHECK(hl_a64_memory_decode(0xa9bf7bfdu, 0x4000, &memory)); /* stp x29,x30,[sp,#-16]! */
    CHECK(memory.kind == HL_A64_MEMORY_PAIR && memory.bytes == 16 && memory.offset == -16);
    CHECK(memory.base == 31 && memory.target == 29 && memory.target2 == 30);
    CHECK(memory.write && !memory.read && memory.writeback && !memory.postindex && memory.stolen);
    CHECK(hl_a64_memory_decode(0xa8c17bfdu, 0x4004, &memory)); /* ldp x29,x30,[sp],#16 */
    CHECK(memory.kind == HL_A64_MEMORY_PAIR && memory.bytes == 16 && memory.offset == 0);
    CHECK(memory.read && !memory.write && memory.writeback && memory.postindex && memory.stolen);
    CHECK(hl_a64_memory_decode(0xf9400020u, 0x4008, &memory)); /* ldr x0,[x1] */
    CHECK(memory.kind == HL_A64_MEMORY_UNSIGNED && memory.bytes == 8 && memory.read && memory.offset == 0);
    CHECK(hl_a64_memory_decode(0xb9000462u, 0x400c, &memory)); /* str w2,[x3,#4] */
    CHECK(memory.kind == HL_A64_MEMORY_UNSIGNED && memory.bytes == 4 && memory.write && memory.offset == 4);
    CHECK(hl_a64_memory_decode(0x58000000u, 0x4010, &memory));
    CHECK(memory.kind == HL_A64_MEMORY_LITERAL && memory.bytes == 8 && memory.pc == 0x4010);
    CHECK(hl_a64_memory_decode(0xd8000000u, 0x4014, &memory));
    CHECK(memory.kind == HL_A64_MEMORY_PREFETCH && memory.bytes == 0 && !memory.read && !memory.write);
    CHECK(hl_a64_memory_decode(0x3dc00020u, 0x4018, &memory)); /* ldr q0,[x1] */
    CHECK(memory.kind == HL_A64_MEMORY_UNSIGNED && memory.bytes == 16 && memory.vector && memory.read);
    const uint32_t qstores[] = {0x3d800023u, 0x3c800023u, 0x3ca26823u};
    const uint32_t kinds[] = {HL_A64_MEMORY_UNSIGNED, HL_A64_MEMORY_UNSCALED, HL_A64_MEMORY_REGISTER};
    for (size_t index = 0; index < 3; ++index) {
        CHECK(hl_a64_memory_decode(qstores[index], 0x4018, &memory));
        CHECK(memory.kind == kinds[index] && memory.bytes == 16 && memory.vector);
        CHECK(memory.write && !memory.read);
    }
    CHECK(hl_a64_memory_decode(0xc85f7c20u, 0x401c, &memory)); /* ldxr x0,[x1] */
    CHECK(memory.kind == HL_A64_MEMORY_EXCLUSIVE && memory.bytes == 8 && memory.read);
    CHECK(!hl_a64_memory_decode(0x91000400u, 0x4020, &memory));
    return 0;
}
