#ifndef HL_CORE_FATAL_H
#define HL_CORE_FATAL_H

#include "hl/host_services.h"

#include <stdatomic.h>

typedef struct hl_fatal_context {
    const hl_host_services *host;
    atomic_int status;
} hl_fatal_context;

void hl_fatal_context_init(hl_fatal_context *context, const hl_host_services *host);
hl_status hl_fatal_report(hl_fatal_context *context, hl_status status, uint32_t tag, const char *message,
                          size_t message_size);
hl_status hl_fatal_status(const hl_fatal_context *context);

#endif
