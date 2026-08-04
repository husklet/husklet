// #120 — RFLAGS.ID (bit 21) round-trip through pushfq/popfq (x86-64 only). Classic CPUID-availability
// probe: software toggles EFLAGS.ID and checks the change stuck. hl models the ID bit in its flag
// substrate (translate.c: popfq stashes bit 21 -> cpu->idflag, pushfq re-materializes it), so a set/clear
// must survive the pushfq/popfq round-trip exactly as real hardware. Byte-exact vs the qemu-x86_64 oracle
// (qemu models EFLAGS.ID correctly), so this is oracle-diffed, not just golden.
#include <stdint.h>
#include <stdio.h>

int main(void) {
    uint64_t after_set, after_clr;
    // Set EFLAGS.ID via popfq, then read it back via pushfq (restoring the caller's flags afterward).
    __asm__ volatile("pushfq\n\t"                  // [orig]
                     "pushfq\n\t"                  // [orig][copy]
                     "movq (%%rsp), %%rax\n\t"
                     "orq $0x200000, %%rax\n\t"    // set ID (bit 21)
                     "movq %%rax, (%%rsp)\n\t"
                     "popfq\n\t"                   // load flags with ID=1
                     "pushfq\n\t"                  // read actual flags back
                     "popq %0\n\t"                 // after_set
                     "popfq\n\t"                   // restore original flags
                     : "=r"(after_set)::"rax", "cc", "memory");
    // Clear EFLAGS.ID the same way.
    __asm__ volatile("pushfq\n\t"
                     "pushfq\n\t"
                     "movq (%%rsp), %%rax\n\t"
                     "andq $0xffffffffffdfffff, %%rax\n\t" // clear ID (bit 21)
                     "movq %%rax, (%%rsp)\n\t"
                     "popfq\n\t"                   // load flags with ID=0
                     "pushfq\n\t"
                     "popq %0\n\t"                 // after_clr
                     "popfq\n\t"
                     : "=r"(after_clr)::"rax", "cc", "memory");
    int set = (int)((after_set >> 21) & 1);
    int clr = (int)((after_clr >> 21) & 1);
    int ok = (set == 1) && (clr == 0);
    printf("rflags-id set=%d clr=%d ok=%d\n", set, clr, ok);
    return 0;
}
