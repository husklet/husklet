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

HL_STATIC_ASSERT(sizeof(hl_log_context) == 16, "log context ABI drifted");
HL_STATIC_ASSERT(offsetof(hl_log_context, enabled_tags) == 8, "log context tags ABI drifted");

HL_API hl_status hl_log_context_init(hl_log_context *context, const hl_host_services *host, const char *selector);
HL_API int hl_log_enabled(const hl_log_context *context, uint32_t tag);
HL_API void hl_log_message(const hl_log_context *context, uint32_t tag, const char *message, size_t message_size);
HL_API void hl_log_format(const hl_log_context *context, uint32_t tag, const char *format, ...);
HL_API const char *hl_log_tag_name(uint32_t tag);

#if defined(HL_ENABLE_LOGGING) && HL_ENABLE_LOGGING
#define HL_LOG(context, tag, message)                                                                                  \
    do {                                                                                                               \
        const hl_log_context *hl_log_macro_context = (context);                                                        \
        uint32_t hl_log_macro_tag = (uint32_t)(tag);                                                                   \
        if (hl_log_enabled(hl_log_macro_context, hl_log_macro_tag))                                                    \
            hl_log_message(hl_log_macro_context, hl_log_macro_tag, (message), sizeof(message) - 1u);                   \
    } while (0)
#define HL_LOGF(context, tag, ...)                                                                                     \
    do {                                                                                                               \
        const hl_log_context *hl_log_macro_context = (context);                                                        \
        uint32_t hl_log_macro_tag = (uint32_t)(tag);                                                                   \
        if (hl_log_enabled(hl_log_macro_context, hl_log_macro_tag))                                                    \
            hl_log_format(hl_log_macro_context, hl_log_macro_tag, __VA_ARGS__);                                        \
    } while (0)
#else
#define HL_LOG(context, tag, message) ((void)0)
#define HL_LOGF(context, tag, ...) ((void)0)
#endif

HL_EXTERN_C_END

#endif
