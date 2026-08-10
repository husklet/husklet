#ifndef HL_HOST_DIRECTORY_H
#define HL_HOST_DIRECTORY_H

#include <stdint.h>

#define HL_HOST_DIRECTORY_ACCESS UINT32_C(0x01)
#define HL_HOST_DIRECTORY_MODIFY UINT32_C(0x02)
#define HL_HOST_DIRECTORY_CREATE UINT32_C(0x04)
#define HL_HOST_DIRECTORY_DELETE UINT32_C(0x08)
#define HL_HOST_DIRECTORY_RENAME UINT32_C(0x10)
#define HL_HOST_DIRECTORY_ATTRIB UINT32_C(0x20)

typedef struct hl_host_directory {
    void *state;
} hl_host_directory;

int hl_host_directory_init(hl_host_directory *directory);
int hl_host_directory_set(hl_host_directory *directory, int descriptor, uint64_t token, uint32_t interests);
int hl_host_directory_remove(hl_host_directory *directory, uint64_t token);
int hl_host_directory_wait(hl_host_directory *directory, uint64_t *token);
int hl_host_directory_descriptor(const hl_host_directory *directory);
int hl_host_directory_relocate(hl_host_directory *directory, int collision);
void hl_host_directory_close(hl_host_directory *directory);
void hl_host_directory_abandon(hl_host_directory *directory);

#endif
