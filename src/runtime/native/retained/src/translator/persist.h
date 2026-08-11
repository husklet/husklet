#ifndef HL_TRANSLATOR_PERSIST_H
#define HL_TRANSLATOR_PERSIST_H

#include "hl/host_services.h"

/* A validated, pinned private cache directory. Artifact operations accept only a
 * single leaf name, so no later transaction can change directory identity or
 * traverse outside it. */
typedef struct hl_persist_directory {
    const hl_host_services *services;
    hl_host_handle handle;
} hl_persist_directory;

int hl_persist_directory_open(hl_persist_directory *directory, const hl_host_services *services, const char *path,
                              int create);
int hl_persist_directory_close(hl_persist_directory *directory);
int hl_persist_load_at(const hl_persist_directory *directory, const char *name, uint64_t limit, void **data,
                       size_t *size);
int hl_persist_store_at(const hl_persist_directory *directory, const char *name, const void *data, size_t size);
int hl_persist_remove_at(const hl_persist_directory *directory, const char *name);

typedef struct hl_persist_cursor {
    const unsigned char *data;
    size_t size;
    size_t offset;
} hl_persist_cursor;

int hl_persist_take(hl_persist_cursor *cursor, void *output, size_t size);

#endif
