#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

static void emit_return(void *page, uint16_t value) {
#if defined(__aarch64__)
    uint32_t code[] = {
        0x52800000u | ((uint32_t)value << 5), /* mov w0, #value */
        0xd65f03c0u,                          /* ret */
    };
#elif defined(__x86_64__)
    unsigned char code[] = {
        0xb8, (unsigned char)value, (unsigned char)(value >> 8), 0, 0, /* mov eax, value */
        0xc3,                                                        /* ret */
    };
#else
#error unsupported architecture
#endif
    memcpy(page, code, sizeof code);
    __builtin___clear_cache(page, (char *)page + sizeof code);
}

int main(void) {
    size_t page_size = (size_t)sysconf(_SC_PAGESIZE);
    unsigned char *mapping = mmap(NULL, page_size * 3, PROT_READ | PROT_WRITE,
                                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (mapping == MAP_FAILED) return 1;
    void *code = mapping + page_size * 2;

    emit_return(code, 7);
    if (mprotect(code, page_size, PROT_READ | PROT_EXEC) != 0) return 2;
    int first = ((int (*)(void))code)();

    if (mprotect(code, page_size, PROT_READ | PROT_WRITE) != 0) return 3;
    emit_return(code, 9);
    if (mprotect(code, page_size, PROT_READ | PROT_EXEC) != 0) return 4;
    int second = ((int (*)(void))code)();

    printf("jit-interior first=%d second=%d\n", first, second);
    return first == 7 && second == 9 ? 0 : 5;
}
