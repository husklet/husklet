#ifndef HL_LINUX_ABI_FORK_CODEC_H
#define HL_LINUX_ABI_FORK_CODEC_H

#include <stddef.h>

int hl_fork_wire_pack_strings(char *output, size_t capacity, size_t *offset, int count, char *const strings[]);
int hl_fork_wire_unpack_strings(const char *input, size_t size, size_t *offset, char **strings, int capacity);

#endif
