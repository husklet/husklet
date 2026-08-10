#ifndef HL_CORE_TARGET_SERVICES_H
#define HL_CORE_TARGET_SERVICES_H

#include "native.h"

typedef struct hl_target_services {
    const hl_host_services *injected;
    hl_native_host *native;
    hl_host_services bound;
} hl_target_services;

void hl_target_services_inject(hl_target_services *target, const hl_host_services *injected);
int hl_target_services_bind(hl_target_services *target);
const hl_host_services *hl_target_services_effective(const hl_target_services *target);
hl_host_services *hl_target_services_bound(hl_target_services *target);
int hl_target_services_make_directory(const hl_target_services *target, const char *path, uint32_t permissions);
void hl_target_services_destroy(hl_target_services *target);

#endif
