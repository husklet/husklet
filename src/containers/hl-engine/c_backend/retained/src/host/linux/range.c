#define _DEFAULT_SOURCE
#include "../range.h"

#include <errno.h>
#include <sys/mman.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

size_t hl_host_page_size(void) {
    long value = sysconf(_SC_PAGESIZE);
    if (value <= 0 || ((size_t)value & ((size_t)value - 1)) != 0) return 0;
    return (size_t)value;
}

int hl_host_address_mapped(uintptr_t address) {
    size_t page_size = hl_host_page_size();
    unsigned char resident;
    uintptr_t page;
    if (page_size == 0) return 0;
    page = address & ~((uintptr_t)page_size - 1);
    // Only ENOMEM means unmapped. mincore() also reports EAGAIN when the kernel is transiently out of
    // resources, which under load turned live guest addresses into spurious EFAULTs.
    for (int attempt = 0; attempt < 64; attempt++) {
        errno = 0;
        if (mincore((void *)page, page_size, &resident) == 0) return 1;
        if (errno != EAGAIN) return errno != ENOMEM;
    }
    return 1;
}

int hl_host_region_query(uintptr_t address, hl_host_region *region) {
    FILE *maps;
    char line[512];
    if (region == NULL) return 0;
    maps = fopen("/proc/self/maps", "r");
    if (maps == NULL) return 0;
    while (fgets(line, sizeof line, maps) != NULL) {
        unsigned long long start;
        unsigned long long end;
        char protection[5] = {0};
        if (sscanf(line, "%llx-%llx %4s", &start, &end, protection) != 3 || end <= start || end <= address) continue;
        if (start > UINTPTR_MAX || end - start > SIZE_MAX) break;
        region->address = (uintptr_t)start;
        region->size = (size_t)(end - start);
        region->protection = (protection[0] == 'r' ? HL_HOST_REGION_READ : 0) |
                             (protection[1] == 'w' ? HL_HOST_REGION_WRITE : 0) |
                             (protection[2] == 'x' ? HL_HOST_REGION_EXECUTE : 0);
        fclose(maps);
        return 1;
    }
    fclose(maps);
    return 0;
}
