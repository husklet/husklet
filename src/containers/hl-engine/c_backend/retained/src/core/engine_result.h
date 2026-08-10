#ifndef HL_CORE_ENGINE_RESULT_H
#define HL_CORE_ENGINE_RESULT_H

#include "hl/host_services.h"

enum {
    HL_ENGINE_CHILD_RESULT_MAGIC = 0x48524c54u,
    HL_ENGINE_CHILD_RESULT_VERSION = 2u,
    HL_ENGINE_CHILD_RESULT_EXIT = 1u,
    HL_ENGINE_CHILD_RESULT_SIGNAL = 2u
};

typedef struct hl_engine_child_result {
    uint32_t magic;
    uint32_t version;
    int32_t guest_status;
    int32_t engine_status;
    uint32_t kind;
    uint32_t reserved;
    uint64_t detail;
} hl_engine_child_result;

void hl_engine_child_result_publish(int32_t guest_status, hl_status engine_status, uint64_t detail);
void hl_engine_child_result_publish_signal(int32_t guest_signal);
void hl_engine_child_result_after_fork(void);

#endif
