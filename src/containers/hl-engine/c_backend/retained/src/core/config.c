#include "hl/config.h"

#include <string.h>

hl_status hl_launch_config_validate(const void *wire, size_t wire_size, hl_launch_config *out_config,
                                    const char **out_pool) {
    hl_launch_config config;
    size_t complete_size;
    if (out_config != NULL) memset(out_config, 0, sizeof(*out_config));
    if (out_pool != NULL) *out_pool = NULL;
    if (wire == NULL || wire_size < sizeof(config)) return HL_STATUS_INVALID_ARGUMENT;
    memset(&config, 0, sizeof(config));
    memcpy(&config, wire, sizeof(config));
    if (config.magic != HL_CONFIG_MAGIC) return HL_STATUS_CORRUPT;
    if (config.abi != HL_CONFIG_ABI) return HL_STATUS_ABI_MISMATCH;
    if (config.header_size < sizeof(config) || config.reserved != 0 || config.reserved2 != 0 ||
        config.checkpoint_policy > HL_CONFIG_CHECKPOINT_REFUSE ||
        (config.checkpoint_mode & ~(HL_CONFIG_CHECKPOINT_CAPTURE | HL_CONFIG_CHECKPOINT_RESTORE)) != 0 ||
        config.network_transport > HL_CONFIG_NETWORK_HOST ||
        config.network_isolated != (config.network_transport == HL_CONFIG_NETWORK_ISOLATED) ||
        config.lower_layer_count > 8 || ((config.lower_layer_count == 0) != (config.lower_layers_offset == 0)) ||
        ((config.lower_layer_count == 0) != (config.overlay_work_offset == 0)))
        return HL_STATUS_CORRUPT;
    if ((config.process_domain[0] | config.process_domain[1]) == 0) return HL_STATUS_INVALID_ARGUMENT;
    if (config.header_size > wire_size) return HL_STATUS_CORRUPT;
    complete_size = (size_t)config.header_size + config.pool_size;
    if (complete_size < config.header_size || complete_size != wire_size) return HL_STATUS_CORRUPT;
    if (config.pool_size == 0 || ((const char *)wire)[config.header_size] != '\0') return HL_STATUS_CORRUPT;
    if (config.lower_layer_count != 0) {
        const char *pool = (const char *)wire + config.header_size;
        uint32_t offset = config.lower_layers_offset;
        for (uint32_t index = 0; index < config.lower_layer_count; index++) {
            const char *end;
            if (offset == 0 || offset >= config.pool_size) return HL_STATUS_CORRUPT;
            end = memchr(pool + offset, '\0', config.pool_size - offset);
            if (end == NULL || end == pool + offset || pool[offset] != '/') return HL_STATUS_CORRUPT;
            offset = (uint32_t)(end - pool) + 1;
        }
    }
    if ((config.publish_count == 0) != (config.publish_offset == 0)) return HL_STATUS_CORRUPT;
    if (config.publish_count != 0) {
        size_t bytes = (size_t)config.publish_count * sizeof(hl_engine_publish_rule);
        if (bytes / sizeof(hl_engine_publish_rule) != config.publish_count || config.publish_offset % 4u != 0 ||
            config.publish_offset >= config.pool_size || bytes > config.pool_size - config.publish_offset)
            return HL_STATUS_CORRUPT;
    }
    if (out_config != NULL) *out_config = config;
    if (out_pool != NULL) *out_pool = (const char *)wire + config.header_size;
    return HL_STATUS_OK;
}

hl_status hl_launch_config_publish(const hl_launch_config *config, const char *pool,
                                   const hl_engine_publish_rule **out_rules) {
    const hl_engine_publish_rule *rules;
    uint32_t index;
    if (out_rules != NULL) *out_rules = NULL;
    if (config == NULL || pool == NULL || config->publish_count == 0 || config->publish_offset == 0)
        return HL_STATUS_INVALID_ARGUMENT;
    rules = (const hl_engine_publish_rule *)(const void *)(pool + config->publish_offset);
    for (index = 0; index < config->publish_count; ++index)
        if (rules[index].host_port == 0 || rules[index].guest_port == 0) return HL_STATUS_CORRUPT;
    if (out_rules != NULL) *out_rules = rules;
    return HL_STATUS_OK;
}

hl_status hl_launch_config_arguments_validate(const hl_launch_config *config, const char *pool, size_t *out_count) {
    const char *cursor;
    const char *end;
    size_t count = 0;
    if (out_count != NULL) *out_count = 0;
    if (config == NULL || pool == NULL || config->arguments_offset == 0 ||
        config->arguments_offset >= config->pool_size)
        return HL_STATUS_INVALID_ARGUMENT;
    cursor = pool + config->arguments_offset;
    end = pool + config->pool_size;
    while (cursor < end && *cursor != '\0') {
        const char *terminator = memchr(cursor, '\0', (size_t)(end - cursor));
        if (terminator == NULL) return HL_STATUS_CORRUPT;
        count++;
        cursor = terminator + 1;
    }
    if (cursor >= end || count == 0) return HL_STATUS_CORRUPT;
    if (out_count != NULL) *out_count = count;
    return HL_STATUS_OK;
}

hl_status hl_launch_config_argument(const hl_launch_config *config, const char *pool, size_t index,
                                    const char **out_argument, size_t *out_size) {
    const char *cursor;
    const char *end;
    size_t current = 0;
    if (out_argument != NULL) *out_argument = NULL;
    if (out_size != NULL) *out_size = 0;
    if (hl_launch_config_arguments_validate(config, pool, NULL) != HL_STATUS_OK) return HL_STATUS_CORRUPT;
    cursor = pool + config->arguments_offset;
    end = pool + config->pool_size;
    while (cursor < end && *cursor != '\0') {
        const char *terminator = memchr(cursor, '\0', (size_t)(end - cursor));
        if (terminator == NULL) return HL_STATUS_CORRUPT;
        if (current == index) {
            if (out_argument != NULL) *out_argument = cursor;
            if (out_size != NULL) *out_size = (size_t)(terminator - cursor);
            return HL_STATUS_OK;
        }
        current++;
        cursor = terminator + 1;
    }
    return HL_STATUS_NOT_FOUND;
}

hl_status hl_launch_config_string(const hl_launch_config *config, const char *pool, uint32_t offset,
                                  const char **out_string, size_t *out_size) {
    const char *end;
    if (out_string != NULL) *out_string = NULL;
    if (out_size != NULL) *out_size = 0;
    if (config == NULL || pool == NULL || offset >= config->pool_size) return HL_STATUS_INVALID_ARGUMENT;
    end = memchr(pool + offset, '\0', config->pool_size - offset);
    if (end == NULL) return HL_STATUS_CORRUPT;
    if (out_string != NULL) *out_string = pool + offset;
    if (out_size != NULL) *out_size = (size_t)(end - (pool + offset));
    return HL_STATUS_OK;
}
