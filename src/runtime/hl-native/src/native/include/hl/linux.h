#ifndef HL_LINUX_H
#define HL_LINUX_H

#include "hl/host_services.h"

HL_EXTERN_C_BEGIN

typedef struct hl_host_linux hl_host_linux;

HL_API hl_status hl_host_linux_create(hl_host_linux **out_host, hl_host_services *out_services);
/* Duplicates a live native descriptor into an owned FILE handle. The caller
 * retains the native descriptor; close the returned handle through services. */
HL_API hl_host_result hl_host_linux_import_file(hl_host_linux *host, int descriptor);
HL_API void hl_host_linux_destroy(hl_host_linux *host);

HL_EXTERN_C_END

#endif
