#include "fatal.h"

void hl_fatal_context_init(hl_fatal_context *context, const hl_host_services *host) {
    if (context == NULL) return;
    context->host = host;
    atomic_store_explicit(&context->status, HL_STATUS_OK, memory_order_relaxed);
}

hl_status hl_fatal_report(hl_fatal_context *context, hl_status status, uint32_t tag, const char *message,
                          size_t message_size) {
    const hl_host_services *host;
    int expected = HL_STATUS_OK;
    if (context == NULL || status == HL_STATUS_OK) return HL_STATUS_INVALID_ARGUMENT;
    if (!atomic_compare_exchange_strong_explicit(&context->status, &expected, status, memory_order_acq_rel,
                                                 memory_order_acquire))
        return (hl_status)expected;
    host = context->host;
#if defined(HL_ENABLE_LOGGING) && HL_ENABLE_LOGGING
    if (host != NULL && host->size >= offsetof(hl_host_services, log) + sizeof(const hl_host_log_services *) &&
        (host->capabilities & HL_HOST_CAP_LOG) != 0 && host->log != NULL && host->log->abi == HL_HOST_LOG_ABI &&
        host->log->size >= sizeof(*host->log) && host->log->emit != NULL && message != NULL)
        host->log->emit(host->context, tag, message, message_size);
#else
    (void)host;
    (void)tag;
    (void)message;
    (void)message_size;
#endif
    return status;
}

hl_status hl_fatal_status(const hl_fatal_context *context) {
    return context == NULL ? HL_STATUS_INVALID_ARGUMENT
                           : (hl_status)atomic_load_explicit(&context->status, memory_order_acquire);
}
