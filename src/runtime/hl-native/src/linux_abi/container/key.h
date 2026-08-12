#ifndef HL_LINUX_ABI_CONTAINER_KEY_H
#define HL_LINUX_ABI_CONTAINER_KEY_H

#include <stddef.h>

/* Format a stable, path-safe identity for a caller-supplied container key. */
int hl_linux_container_key(const char *input, char *output, size_t capacity);

#endif
