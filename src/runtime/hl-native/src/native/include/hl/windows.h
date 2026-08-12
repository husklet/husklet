#ifndef HL_WINDOWS_H
#define HL_WINDOWS_H

#include "hl/host_services.h"

HL_EXTERN_C_BEGIN

typedef struct hl_host_windows hl_host_windows;

HL_API hl_status hl_host_windows_create(hl_host_windows **out_host, hl_host_services *out_services);
/* Duplicates a live CRT descriptor into an owned FILE handle. */
HL_API hl_host_result hl_host_windows_import_file(hl_host_windows *host, int descriptor, uint32_t access);
HL_API void hl_host_windows_destroy(hl_host_windows *host);

HL_EXTERN_C_END

#endif
