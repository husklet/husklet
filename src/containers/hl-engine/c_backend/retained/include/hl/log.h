#ifndef HL_LOG_H
#define HL_LOG_H

#include "hl/host_services.h"

HL_EXTERN_C_BEGIN

typedef enum hl_log_tag {
    HL_LOG_TAG_FS = 1u << 0,
    HL_LOG_TAG_JIT = 1u << 1,
    HL_LOG_TAG_SYSCALL = 1u << 2,
    HL_LOG_TAG_PROCESS = 1u << 3,
    HL_LOG_TAG_NETWORK = 1u << 4,
    HL_LOG_TAG_SIGNAL = 1u << 5,
    HL_LOG_TAG_TRANSLATE = 1u << 6,
    HL_LOG_TAG_ALL = (1u << 8) - 1u
} hl_log_tag;

typedef struct hl_log_context {
    const hl_host_services *host;
    uint32_t enabled_tags;
    uint32_t reserved;
} hl_log_context;

HL_API hl_status hl_log_context_init(hl_log_context *context, const hl_host_services *host, const char *selector);
HL_API int hl_log_enabled(const hl_log_context *context, uint32_t tag);
HL_API void hl_log_message(const hl_log_context *context, uint32_t tag, const char *message, size_t message_size);
HL_API void hl_log_format(const hl_log_context *context, uint32_t tag, const char *format, ...);
HL_API const char *hl_log_tag_name(uint32_t tag);

#if defined(HL_ENABLE_LOGGING) && HL_ENABLE_LOGGING
#define HL_LOG(context, tag, message) hl_log_message((context), (tag), (message), sizeof(message) - 1u)
#define HL_LOGF(context, tag, ...) hl_log_format((context), (tag), __VA_ARGS__)
#else
#define HL_LOG(context, tag, message) ((void)0)
#define HL_LOGF(context, tag, ...) ((void)0)
#endif

HL_EXTERN_C_END

#endif
