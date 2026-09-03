#include <stdint.h>
#include <stdio.h>

__attribute__((naked, noinline, visibility("hidden"))) uint64_t addr32_call_target(uint64_t value) {
    (void)value;
    __asm__ volatile("lea 7(%rdi,%rdi,2),%rax\n\tret");
}

__attribute__((naked, noinline)) static uint64_t ordinary(uint64_t value) {
    (void)value;
    __asm__ volatile(".byte 0x67,0xe8\n\t"
                     ".long addr32_call_target-1f\n\t"
                     "1: ret");
}

// The six-byte 67 E8 rel32 starts three bytes before a page boundary, so decoding its immediate requires
// both executable pages.  This is the same lowering as ordinary(), not a fixture-only aligned fast path.
__attribute__((naked, noinline, aligned(4096))) static uint64_t page_crossing(uint64_t value) {
    (void)value;
    __asm__ volatile(".fill 4093,1,0x90\n\t"
                     ".byte 0x67,0xe8\n\t"
                     ".long addr32_call_target-1f\n\t"
                     "1: ret");
}

int main(void) {
    uint64_t ordinary_sum = 0;
    uint64_t crossing_sum = 0;
    for (uint64_t value = 1; value <= 128; value++) {
        ordinary_sum += ordinary(value);
        crossing_sum += page_crossing(value);
    }
    printf("addr32-call ordinary=%llu crossing=%llu\n", (unsigned long long)ordinary_sum,
           (unsigned long long)crossing_sum);
    return ordinary_sum == 25664 && crossing_sum == ordinary_sum ? 0 : 1;
}
