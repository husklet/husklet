#include "range.h"

int hl_host_range_mapped(uintptr_t address, size_t size) {
    size_t page_size;
    uintptr_t end;
    uintptr_t page;
    if (size == 0) return 1;
    end = address + size;
    if (end < address) return 0;
    page_size = hl_host_page_size();
    if (page_size == 0) return 0;
    page = address & ~((uintptr_t)page_size - 1);
    for (;;) {
        if (!hl_host_address_mapped(page)) return 0;
        if (end - 1 - page < page_size) return 1;
        page += page_size;
    }
}

int hl_host_page_neighbor_mapped(uintptr_t page_address) {
    size_t page_size = hl_host_page_size();
    if (page_size == 0 || (page_address & ((uintptr_t)page_size - 1)) != 0) return 0;
    if (page_address != 0 && hl_host_address_mapped(page_address - 1)) return 1;
    if (page_address <= UINTPTR_MAX - page_size && hl_host_address_mapped(page_address + page_size)) return 1;
    return 0;
}
