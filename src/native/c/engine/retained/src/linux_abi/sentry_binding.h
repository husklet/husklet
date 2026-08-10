#ifndef HL_LINUX_ABI_SENTRY_BINDING_H
#define HL_LINUX_ABI_SENTRY_BINDING_H

#include <errno.h>
#include <stddef.h>
#include <stdint.h>

#define HL_SENTRY_BINDING_CAPACITY 4096u

struct hl_sentry_binding {
    int32_t owner;
    uint32_t token;
    uint16_t table;
    uint8_t inuse;
};

static inline struct hl_sentry_binding *hl_sentry_binding_find(
    struct hl_sentry_binding *bindings, uint32_t count, int32_t owner, uint32_t token) {
    for (uint32_t i = 0; i < count; i++)
        if (bindings[i].inuse && bindings[i].owner == owner && bindings[i].token == token)
            return &bindings[i];
    return NULL;
}

static inline int hl_sentry_binding_reserve(
    struct hl_sentry_binding *bindings, uint32_t count, int32_t owner, uint32_t token,
    uint16_t table) {
    if (token == 0) return -EINVAL;
    if (hl_sentry_binding_find(bindings, count, owner, token)) return -EEXIST;
    for (uint32_t i = 0; i < count; i++)
        if (!bindings[i].inuse) {
            bindings[i] = (struct hl_sentry_binding){
                .owner = owner,
                .token = token,
                .table = table,
                .inuse = 1,
            };
            return 0;
        }
    return -EAGAIN;
}

static inline int hl_sentry_binding_release(
    struct hl_sentry_binding *bindings, uint32_t count, int32_t owner, uint32_t token,
    uint16_t *table) {
    struct hl_sentry_binding *binding =
        hl_sentry_binding_find(bindings, count, owner, token);
    if (!binding) return -ENOENT;
    *table = binding->table;
    *binding = (struct hl_sentry_binding){0};
    return 0;
}

#endif
