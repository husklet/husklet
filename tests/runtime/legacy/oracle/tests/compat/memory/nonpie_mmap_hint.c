#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

static int still_executable(int value) { return value * 3 + 1; }

int main(void) {
    long page = sysconf(_SC_PAGESIZE);
    uintptr_t hint = (uintptr_t)&still_executable & ~((uintptr_t)page - 1);
    void *mapped = mmap((void *)hint, (size_t)page, PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    int relocated = mapped != MAP_FAILED && mapped != (void *)hint;
    int code_ok = still_executable(7) == 22;
    if (mapped != MAP_FAILED) munmap(mapped, (size_t)page);
    printf("nonpie-mmap-hint relocated=%d code_ok=%d\n", relocated, code_ok);
    return relocated && code_ok ? 0 : 1;
}
