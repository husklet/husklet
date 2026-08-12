#ifndef HL_LINUX_ABI_MAPPING_OUTPUT_H
#define HL_LINUX_ABI_MAPPING_OUTPUT_H

#include "hl/linux_abi.h"

static inline int hl_linux_file_mapping_output_prepare(hl_host_file_mapping *mapping) {
    if (mapping == NULL || mapping->abi != HL_HOST_FILE_MAPPING_ABI || mapping->size < sizeof(*mapping)) return 0;
    mapping->handle = HL_HOST_HANDLE_INVALID;
    mapping->address = 0;
    mapping->mapped_size = 0;
    mapping->reserved = 0;
    return 1;
}

#endif
