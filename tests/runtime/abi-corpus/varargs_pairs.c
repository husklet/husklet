#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

static void *pairs(unsigned flags, ...) {
    va_list args;
    void **slot;
    size_t total = 0;
    va_start(args, flags);
    while ((slot = va_arg(args, void **)) != NULL) total += (va_arg(args, unsigned) + 7u) & ~7u;
    va_end(args);
    unsigned char *base = malloc(total), *next = base;
    if (base == NULL) return NULL;
    va_start(args, flags);
    while ((slot = va_arg(args, void **)) != NULL) {
        *slot = next;
        next += (va_arg(args, unsigned) + 7u) & ~7u;
    }
    va_end(args);
    return base;
}

int main(void) {
    void *p[12] = {0};
    void *base = pairs(3, &p[0], 1u, &p[1], 17u, &p[2], 3u, &p[3], 257u, &p[4], 9u, &p[5], 65u,
                       &p[6], 2u, &p[7], 513u, &p[8], 5u, &p[9], 33u, &p[10], 7u, &p[11], 129u, NULL);
    int ok = base != NULL;
    static const size_t expected[] = {0, 8, 32, 40, 304, 320, 392, 400, 920, 928, 968, 976};
    for (size_t i = 0; i < 12; ++i) ok &= (uintptr_t)p[i] - (uintptr_t)base == expected[i];
    printf("varargs-pairs ok=%d total=%zu\n", ok, (uintptr_t)p[11] + 136u - (uintptr_t)base);
    free(base);
    return ok ? 0 : 1;
}
