// #425: models V8's embedded-builtins code-range invariant in a non-PIE ET_EXEC. hl maps the image HIGH
// (+bias) but keeps guest-visible pointers LOW; V8 loads its embedded-blob CODE base (symbol
// v8_Default_embedded_blob_code_) via `mov r,imm` and later range-checks a stack RETURN ADDRESS (which is
// HIGH, where code executes) against it. If the base stays LOW, InnerPointerToCodeCache misses -> V8_Fatal
// (node:20 `new Error().stack` / mongosh). The engine records that exact symbol and rebases its baked mov-imm
// materialization to the high mapping, WITHOUT changing return addresses (Go's HIGH-PC model stays intact).
// This guard reproduces the shape: the mov-imm'd code base and the execution return address must live in the
// SAME address-space half. qemu -> both low; hl fixed -> both high; hl unfixed -> base low, ret high (0).
// Diffed byte-for-byte vs qemu-x86_64. x86-only. (node/mongosh use the B8-BF encoding; gcc here emits C7/0 --
// the engine rebases both forms, so this guards the C7 path while the real workloads exercise B8-BF.)
#include <stdint.h>
#include <stdio.h>

__attribute__((used, noinline)) void v8_Default_embedded_blob_code_(void) { __asm__ volatile(""); }

static volatile uintptr_t g_ret;
__attribute__((noinline)) static void capture(void) { g_ret = (uintptr_t)__builtin_return_address(0); }

int main(void) {
    capture(); // g_ret = a return address into main -- where code actually executes (HIGH under hl)
    uintptr_t base; // gcc emits `mov r64,imm32` (C7 /0) for the symbol address in a non-PIE build
    __asm__ volatile("mov %1, %0" : "=r"(base) : "i"(&v8_Default_embedded_blob_code_));
    printf("same_half=%d\n", (int)((base >> 32) == (g_ret >> 32)));
    return 0;
}
