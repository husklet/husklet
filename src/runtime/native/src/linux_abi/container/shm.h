#ifndef HL_LINUX_ABI_CONTAINER_SHM_H
#define HL_LINUX_ABI_CONTAINER_SHM_H
#include <stddef.h>
const char *hl_shm_path(const char *guest, const char *root, const char *namespace_key, char *output, size_t capacity);
#endif
