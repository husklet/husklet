#include "key.h"

#include <stdint.h>
#include <stdio.h>

int hl_linux_container_key(const char *input, char *output, size_t capacity) {
    uint64_t hash = UINT64_C(14695981039346656037);
    const unsigned char *cursor;

    if (input == NULL || output == NULL || capacity < 17) return -1;
    cursor = (const unsigned char *)input;
    while (*cursor != 0) {
        hash ^= *cursor++;
        hash *= UINT64_C(1099511628211);
    }
    return snprintf(output, capacity, "%016llx", (unsigned long long)hash) == 16 ? 0 : -1;
}
