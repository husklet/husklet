#ifndef HL_C_BRIDGE_HOST_H
#define HL_C_BRIDGE_HOST_H

#include "hl/host_services.h"

typedef struct hl_c_bridge_host hl_c_bridge_host;

hl_status hl_c_bridge_host_create(hl_c_bridge_host **out_host, hl_host_services *out_services);
hl_host_result hl_c_bridge_host_import_file(hl_c_bridge_host *host, int32_t descriptor, uint32_t access);
void hl_c_bridge_host_destroy(hl_c_bridge_host *host);

#endif
