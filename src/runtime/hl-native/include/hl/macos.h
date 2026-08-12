#ifndef HL_MACOS_H
#define HL_MACOS_H

#include "hl/host_services.h"

HL_EXTERN_C_BEGIN

typedef struct hl_host_macos hl_host_macos;

HL_API hl_status hl_host_macos_create(hl_host_macos **out_host, hl_host_services *out_services);
HL_API void hl_host_macos_destroy(hl_host_macos *host);

HL_EXTERN_C_END

#endif
