#ifndef HL_CORE_ENGINE_RESULT_H
#define HL_CORE_ENGINE_RESULT_H

#include "hl/host_services.h"

enum {
    HL_ENGINE_CHILD_RESULT_MAGIC = 0x48524c54u,
    HL_ENGINE_CHILD_RESULT_VERSION = 4u,
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
    uint64_t translations;
    /* The container-namespace pid of the guest process this engine launched, published by the child
       once its container identity exists and readable by the parent for the whole run. It is the one
       identity a checkpoint preserves: the image names each captured member `proc.<guest pid>` and a
       restore re-forks it under exactly this number, so it is what a host holding a captured member
       may key on. Zero until the child publishes, and never republished. */
    int32_t guest_pid;
    int32_t guest_pid_reserved;
} hl_engine_child_result;

void hl_engine_child_result_publish(int32_t guest_status, hl_status engine_status, uint64_t detail);
void hl_engine_child_result_publish_signal(int32_t guest_signal);
void hl_engine_child_result_publish_guest_pid(int32_t guest_pid);
void hl_engine_child_result_after_fork(void);
#if defined(HL_NATIVE_TEST_HOOKS)
void hl_engine_child_result_begin_for_test(hl_engine_child_result *record);
void hl_engine_child_result_end_for_test(void);
#endif

#endif
