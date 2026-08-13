#include "hl/log.h"

#include <stdarg.h>
#include <stdio.h>
#include <string.h>

typedef struct hl_log_tag_entry {
    const char *name;
    uint32_t tag;
} hl_log_tag_entry;

static const hl_log_tag_entry hl_log_tags[] = {
    {"fs", HL_LOG_TAG_FS},
    {"jit", HL_LOG_TAG_JIT},
    {"syscall", HL_LOG_TAG_SYSCALL},
    {"process", HL_LOG_TAG_PROCESS},
    {"network", HL_LOG_TAG_NETWORK},
    {"signal", HL_LOG_TAG_SIGNAL},
    {"translate", HL_LOG_TAG_TRANSLATE},
};

const char *hl_log_tag_name(uint32_t tag) {
    size_t index;
    for (index = 0; index < HL_ARRAY_COUNT(hl_log_tags); index++)
        if (hl_log_tags[index].tag == tag) return hl_log_tags[index].name;
    return "unknown";
}

#if defined(HL_ENABLE_LOGGING) && HL_ENABLE_LOGGING
static uint32_t hl_log_parse(const char *selector) {
    uint32_t result = 0;
    const char *cursor = selector;
    while (cursor != NULL && *cursor != '\0') {
        const char *end = strchr(cursor, ',');
        size_t size = end != NULL ? (size_t)(end - cursor) : strlen(cursor);
        size_t index;
        if (size > 4 && memcmp(cursor, "log:", 4) == 0) {
            cursor += 4;
            size -= 4;
        }
        if (size == 3 && memcmp(cursor, "all", 3) == 0) result |= HL_LOG_TAG_ALL;
        for (index = 0; index < HL_ARRAY_COUNT(hl_log_tags); index++)
            if (strlen(hl_log_tags[index].name) == size && memcmp(cursor, hl_log_tags[index].name, size) == 0)
                result |= hl_log_tags[index].tag;
        cursor = end != NULL ? end + 1 : NULL;
    }
    return result;
}
#endif

hl_status hl_log_context_init(hl_log_context *context, const hl_host_services *host, const char *selector) {
    if (context == NULL || host == NULL) return HL_STATUS_INVALID_ARGUMENT;
    memset(context, 0, sizeof(*context));
    context->host = host;
#if defined(HL_ENABLE_LOGGING) && HL_ENABLE_LOGGING
    if ((host->capabilities & HL_HOST_CAP_LOG) == 0 || host->log == NULL || host->log->emit == NULL)
        return HL_STATUS_NOT_SUPPORTED;
    context->enabled_tags = hl_log_parse(selector);
#else
    (void)selector;
#endif
    return HL_STATUS_OK;
}

int hl_log_enabled(const hl_log_context *context, uint32_t tag) {
#if defined(HL_ENABLE_LOGGING) && HL_ENABLE_LOGGING
    return context != NULL && (context->enabled_tags & tag) != 0;
#else
    (void)context;
    (void)tag;
    return 0;
#endif
}

void hl_log_message(const hl_log_context *context, uint32_t tag, const char *message, size_t message_size) {
#if defined(HL_ENABLE_LOGGING) && HL_ENABLE_LOGGING
    char output[1200];
    int prefix;
    size_t copy;
    if (!hl_log_enabled(context, tag) || message == NULL) return;
    prefix = snprintf(output, sizeof output, "[hl:%s] ", hl_log_tag_name(tag));
    if (prefix < 0 || (size_t)prefix >= sizeof output) return;
    copy = message_size < sizeof output - (size_t)prefix - 1u ? message_size : sizeof output - (size_t)prefix - 1u;
    memcpy(output + prefix, message, copy);
    output[(size_t)prefix + copy] = '\n';
    context->host->log->emit(context->host->context, tag, output, (size_t)prefix + copy + 1u);
#else
    (void)context;
    (void)tag;
    (void)message;
    (void)message_size;
#endif
}

void hl_log_format(const hl_log_context *context, uint32_t tag, const char *format, ...) {
#if defined(HL_ENABLE_LOGGING) && HL_ENABLE_LOGGING
    char message[1024];
    int count;
    va_list arguments;
    if (!hl_log_enabled(context, tag) || format == NULL) return;
    va_start(arguments, format);
    count = vsnprintf(message, sizeof message, format, arguments);
    va_end(arguments);
    if (count < 0) return;
    hl_log_message(context, tag, message, (size_t)count < sizeof message ? (size_t)count : sizeof message - 1u);
#else
    (void)context;
    (void)tag;
    (void)format;
#endif
}

void hl_log_guest_fatal(const hl_log_context *context, uint32_t signal, uint64_t pc, uint64_t sp, uint64_t lr) {
#if defined(HL_ENABLE_LOGGING) && HL_ENABLE_LOGGING
    static const char prefix[] = "[hl:signal] fatal-guest-signal signal=";
    static const char pc_label[] = " pc=0x";
    static const char sp_label[] = " sp=0x";
    static const char lr_label[] = " lr=0x";
    static const char digits[] = "0123456789abcdef";
    char output[160];
    char reverse[20];
    size_t size = 0;
    size_t index;
    uint64_t value;
    if (!hl_log_enabled(context, HL_LOG_TAG_SIGNAL)) return;
    for (index = 0; index < sizeof(prefix) - 1u; ++index) output[size++] = prefix[index];
    value = signal;
    index = 0;
    do {
        reverse[index++] = (char)('0' + value % 10u);
        value /= 10u;
    } while (value != 0);
    while (index != 0) output[size++] = reverse[--index];
#define HL_FATAL_HEX(label, number)                                                                                  \
    do {                                                                                                             \
        size_t label_index;                                                                                          \
        size_t shift = 60u;                                                                                          \
        int emitted = 0;                                                                                             \
        for (label_index = 0; label_index < sizeof(label) - 1u; ++label_index) output[size++] = (label)[label_index]; \
        do {                                                                                                         \
            unsigned nibble = (unsigned)(((number) >> shift) & 0xfu);                                                \
            if (nibble != 0 || emitted || shift == 0) {                                                              \
                output[size++] = digits[nibble];                                                                     \
                emitted = 1;                                                                                         \
            }                                                                                                        \
            if (shift == 0) break;                                                                                   \
            shift -= 4u;                                                                                             \
        } while (1);                                                                                                 \
    } while (0)
    HL_FATAL_HEX(pc_label, pc);
    HL_FATAL_HEX(sp_label, sp);
    HL_FATAL_HEX(lr_label, lr);
#undef HL_FATAL_HEX
    output[size++] = '\n';
    context->host->log->emit(context->host->context, HL_LOG_TAG_SIGNAL, output, size);
#else
    (void)context;
    (void)signal;
    (void)pc;
    (void)sp;
    (void)lr;
#endif
}
