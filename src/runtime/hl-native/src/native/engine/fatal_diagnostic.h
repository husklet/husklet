#ifndef HL_FATAL_DIAGNOSTIC_H
#define HL_FATAL_DIAGNOSTIC_H

#include "hl/log.h"

void hl_fatal_diagnostic_init(const hl_host_services *host, const char *selector);
void hl_fatal_diagnostic_publish(uint32_t signal, uint64_t pc, uint64_t sp, uint64_t lr);

#endif
