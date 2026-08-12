#ifndef HL_NETWORK_H
#define HL_NETWORK_H

#include "hl/base.h"

HL_EXTERN_C_BEGIN

/* One exact host IPv4 TCP/UDP publication. Zero host_ipv4_be binds every host address. */
typedef struct hl_engine_publish_rule {
    uint32_t host_ipv4_be;
    uint16_t host_port;
    uint16_t guest_port;
} hl_engine_publish_rule;

HL_EXTERN_C_END

#endif
