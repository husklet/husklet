#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>

int main(void) {
    const size_t instructions = 17u * 1024u * 1024u;
    const size_t bytes = (instructions + 1u) * sizeof(uint32_t);
    uint32_t *code = mmap(NULL, bytes, PROT_READ | PROT_WRITE | PROT_EXEC,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (code == MAP_FAILED) return 1;

    for (size_t i = 0; i < instructions; i++) code[i] = UINT32_C(0xd503201f); /* nop */
    code[instructions] = UINT32_C(0xd65f03c0);                               /* ret */
    __builtin___clear_cache((char *)code, (char *)code + bytes);

    ((void (*)(void))code)();
    puts("codecache-straightline ok");
    return 0;
}
