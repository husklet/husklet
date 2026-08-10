#ifndef HL_CORE_PROVIDER_NAMESPACE_H
#define HL_CORE_PROVIDER_NAMESPACE_H

#include <stddef.h>
#include <stdint.h>

enum { HL_PROVIDER_NAMESPACE_MAX = 64, HL_PROVIDER_PATH_MAX = 4096 };

enum {
    HL_PROVIDER_NODE_SERVICE = 1,
    HL_PROVIDER_NODE_DIRECTORY = 2,
    HL_PROVIDER_NODE_SYMLINK = 3,
    HL_PROVIDER_NODE_CHARACTER = 4,
    HL_PROVIDER_NODE_BLOCK = 5
};

typedef struct hl_provider_node {
    uint64_t service;
    uint8_t kind;
    uint32_t mode;
    uint32_t uid;
    uint32_t gid;
    uint16_t path_size;
    char path[HL_PROVIDER_PATH_MAX];
    uint16_t target_size;
    char target[HL_PROVIDER_PATH_MAX];
    uint32_t major;
    uint32_t minor;
} hl_provider_node;

typedef struct hl_provider_namespace {
    uint64_t generation;
    uint32_t count;
    hl_provider_node nodes[HL_PROVIDER_NAMESPACE_MAX];
} hl_provider_namespace;

/* Decodes and validates into temporary storage, committing atomically only on complete success. */
int hl_provider_namespace_install(hl_provider_namespace *namespace, const void *bytes, size_t size,
                                  uint32_t maximum_entries, uint32_t maximum_path);
const hl_provider_node *hl_provider_namespace_resolve(const hl_provider_namespace *namespace, const char *path,
                                                      size_t path_size);
void hl_provider_namespace_revoke(hl_provider_namespace *namespace);
int hl_provider_namespace_launch_install(const void *bytes, size_t size);
const hl_provider_node *hl_provider_namespace_launch_resolve(const char *path, size_t path_size);
const hl_provider_node *hl_provider_namespace_launch_child(const char *directory, size_t directory_size,
                                                           uint32_t *cursor);
void hl_provider_namespace_launch_revoke(void);

#endif
