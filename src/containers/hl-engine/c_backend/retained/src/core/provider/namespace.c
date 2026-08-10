#include "namespace.h"

#include <errno.h>
#include <stdlib.h>
#include <string.h>

typedef struct input {
    const unsigned char *bytes;
    size_t size;
    size_t offset;
} input;

static hl_provider_namespace launch_namespace;

static int take(input *value, size_t count, const unsigned char **out) {
    if (count > value->size - value->offset) return -EPROTO;
    *out = value->bytes + value->offset;
    value->offset += count;
    return 0;
}

static int u16(input *value, uint16_t *out) {
    const unsigned char *bytes;
    if (take(value, 2, &bytes) != 0) return -EPROTO;
    *out = (uint16_t)((uint16_t)bytes[0] | (uint16_t)((uint16_t)bytes[1] << 8));
    return 0;
}

static int u8(input *value, uint8_t *out) {
    const unsigned char *bytes;
    if (take(value, 1, &bytes) != 0) return -EPROTO;
    *out = bytes[0];
    return 0;
}

static int u32(input *value, uint32_t *out) {
    const unsigned char *bytes;
    if (take(value, 4, &bytes) != 0) return -EPROTO;
    *out = (uint32_t)bytes[0] | (uint32_t)bytes[1] << 8 | (uint32_t)bytes[2] << 16 | (uint32_t)bytes[3] << 24;
    return 0;
}

static int u64(input *value, uint64_t *out) {
    uint32_t low, high;
    if (u32(value, &low) != 0 || u32(value, &high) != 0) return -EPROTO;
    *out = (uint64_t)low | (uint64_t)high << 32;
    return 0;
}

static int valid_path(const char *path, size_t size) {
    size_t index, segment = 1;
    if (size < 2 || path[0] != '/' || path[size - 1] == '/') return 0;
    for (index = 1; index <= size; ++index) {
        if (index < size && path[index] != '/') {
            if (path[index] == 0) return 0;
            continue;
        }
        if (index == segment || (index - segment == 1 && path[segment] == '.') ||
            (index - segment == 2 && path[segment] == '.' && path[segment + 1] == '.'))
            return 0;
        segment = index + 1;
    }
    return 1;
}

static int prefix(const hl_provider_node *parent, const hl_provider_node *child) {
    return parent->path_size < child->path_size && child->path[parent->path_size] == '/' &&
           memcmp(parent->path, child->path, parent->path_size) == 0;
}

static int conflicts(const hl_provider_namespace *namespace, const hl_provider_node *node) {
    uint32_t index;
    for (index = 0; index < namespace->count; ++index) {
        const hl_provider_node *other = &namespace->nodes[index];
        if (node->path_size == other->path_size && memcmp(node->path, other->path, node->path_size) == 0) return 1;
        if ((prefix(other, node) && other->kind != HL_PROVIDER_NODE_DIRECTORY) ||
            (prefix(node, other) && node->kind != HL_PROVIDER_NODE_DIRECTORY))
            return 1;
    }
    return 0;
}

int hl_provider_namespace_install(hl_provider_namespace *namespace, const void *bytes, size_t size,
                                  uint32_t maximum_entries, uint32_t maximum_path) {
    hl_provider_namespace *pending;
    input source = {.bytes = bytes, .size = size, .offset = 0};
    uint32_t count, index;
    int status = -EPROTO;
    if (namespace == NULL || bytes == NULL || maximum_entries > HL_PROVIDER_NAMESPACE_MAX || maximum_path == 0 ||
        maximum_path > HL_PROVIDER_PATH_MAX || u32(&source, &count) != 0)
        return -EINVAL;
    if ((count & UINT32_C(0xc0000000)) != UINT32_C(0xc0000000)) return -EPROTO;
    count &= UINT32_C(0x3fffffff);
    if (count > maximum_entries) return -E2BIG;
    pending = calloc(1, sizeof(*pending));
    if (pending == NULL) return -ENOMEM;
    pending->count = count;
    for (index = 0; index < count; ++index) {
        hl_provider_node *node = &pending->nodes[index];
        const unsigned char *path;
        const unsigned char *target;
        if (u8(&source, &node->kind) != 0 || u64(&source, &node->service) != 0 || u32(&source, &node->mode) != 0 ||
            u32(&source, &node->uid) != 0 || u32(&source, &node->gid) != 0 || u16(&source, &node->path_size) != 0 ||
            (node->kind != HL_PROVIDER_NODE_SERVICE && node->kind != HL_PROVIDER_NODE_DIRECTORY &&
             node->kind != HL_PROVIDER_NODE_SYMLINK && node->kind != HL_PROVIDER_NODE_CHARACTER &&
             node->kind != HL_PROVIDER_NODE_BLOCK) ||
            ((node->kind == HL_PROVIDER_NODE_SERVICE || node->kind == HL_PROVIDER_NODE_CHARACTER ||
              node->kind == HL_PROVIDER_NODE_BLOCK)
                 ? node->service == 0
                 : node->service != 0) ||
            (node->mode & ~07777u) != 0 || node->path_size > maximum_path || take(&source, node->path_size, &path) != 0)
            goto done;
        memcpy(node->path, path, node->path_size);
        if (!valid_path(node->path, node->path_size)) {
            status = -EINVAL;
            goto done;
        }
        if (u16(&source, &node->target_size) != 0 || node->target_size > maximum_path ||
            take(&source, node->target_size, &target) != 0)
            goto done;
        if (node->target_size != 0) memcpy(node->target, target, node->target_size);
        if (u32(&source, &node->major) != 0 || u32(&source, &node->minor) != 0) goto done;
        if ((node->kind == HL_PROVIDER_NODE_SYMLINK) != (node->target_size != 0) ||
            (node->target_size != 0 && memchr(node->target, 0, node->target_size) != NULL)) {
            status = -EINVAL;
            goto done;
        }
        if ((node->kind == HL_PROVIDER_NODE_CHARACTER || node->kind == HL_PROVIDER_NODE_BLOCK) &&
            (node->major >= 4096 || node->minor >= (1u << 20))) {
            status = -EINVAL;
            goto done;
        }
        pending->count = index;
        if (conflicts(pending, node)) {
            status = -EEXIST;
            goto done;
        }
        pending->count = index + 1;
    }
    if (source.offset != source.size) goto done;
    pending->generation = namespace->generation + 1;
    if (pending->generation == 0) pending->generation = 1;
    memcpy(namespace, pending, sizeof(*namespace));
    status = 0;
done:
    free(pending);
    return status;
}

const hl_provider_node *hl_provider_namespace_resolve(const hl_provider_namespace *namespace, const char *path,
                                                      size_t path_size) {
    uint32_t index;
    if (namespace == NULL || path == NULL) return NULL;
    for (index = 0; index < namespace->count; ++index)
        if (namespace->nodes[index].path_size == path_size &&
            memcmp(namespace->nodes[index].path, path, path_size) == 0)
            return &namespace->nodes[index];
    return NULL;
}

void hl_provider_namespace_revoke(hl_provider_namespace *namespace) {
    uint64_t generation;
    if (namespace == NULL) return;
    generation = namespace->generation + 1;
    memset(namespace, 0, sizeof(*namespace));
    namespace->generation = generation == 0 ? 1 : generation;
}

int hl_provider_namespace_launch_install(const void *bytes, size_t size) {
    return hl_provider_namespace_install(&launch_namespace, bytes, size, HL_PROVIDER_NAMESPACE_MAX,
                                         HL_PROVIDER_PATH_MAX);
}

const hl_provider_node *hl_provider_namespace_launch_resolve(const char *path, size_t path_size) {
    return hl_provider_namespace_resolve(&launch_namespace, path, path_size);
}

const hl_provider_node *hl_provider_namespace_launch_child(const char *directory, size_t directory_size,
                                                           uint32_t *cursor) {
    uint32_t index;
    if (directory == NULL || cursor == NULL || directory_size == 0) return NULL;
    for (index = *cursor; index < launch_namespace.count; ++index) {
        const hl_provider_node *node = &launch_namespace.nodes[index];
        size_t prefix_size = directory_size;
        const char *child;
        size_t child_size;
        if (directory_size == 1 && directory[0] == '/')
            prefix_size = 0;
        else if (node->path_size <= directory_size || node->path[directory_size] != '/')
            continue;
        if (prefix_size != 0 && memcmp(node->path, directory, prefix_size) != 0) continue;
        child = node->path + prefix_size + 1;
        child_size = node->path_size - prefix_size - 1;
        if (child_size == 0 || memchr(child, '/', child_size) != NULL) continue;
        *cursor = index + 1;
        return node;
    }
    *cursor = launch_namespace.count;
    return NULL;
}

void hl_provider_namespace_launch_revoke(void) {
    hl_provider_namespace_revoke(&launch_namespace);
}
