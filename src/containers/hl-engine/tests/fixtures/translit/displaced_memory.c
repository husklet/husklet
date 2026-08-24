// Exact address-domain clamps for displaced ET_EXEC stage-one transliteration.
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static volatile uint64_t cell = UINT64_C(0x1122334455667788);

__attribute__((noinline)) static uint64_t base_load(const volatile uint64_t *pointer) {
    uint64_t value;
    __asm__ volatile("mov (%1),%0" : "=r"(value) : "r"(pointer) : "memory");
    return value;
}

int main(void) {
    const void *identity;
    uint64_t before;
    __asm__ volatile("lea cell(%%rip),%0" : "=r"(identity));
    __asm__ volatile("mov cell(%%rip),%0" : "=a"(before) : : "memory");
    __asm__ volatile("addq $1,cell(%%rip)" : : : "cc", "memory");
    char source[8] = "domain", destination[8] = {0};
    void *to = destination;
    const void *from = source;
    size_t length = sizeof source;
    __asm__ volatile("rep movsb" : "+D"(to), "+S"(from), "+c"(length) : : "memory");
    printf("displaced ptr=%p before=%016llx base=%016llx after=%016llx text=%s\n", identity,
           (unsigned long long)before, (unsigned long long)base_load(&cell), (unsigned long long)cell, destination);
    return strcmp(destination, source) != 0;
}
