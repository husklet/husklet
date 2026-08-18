#include "api.h"

static const hl_c_bridge_api HL_C_BRIDGE_API = {
    .abi = HL_C_BRIDGE_API_ABI,
    .size = sizeof(hl_c_bridge_api),
    .engine_abi = hl_engine_abi,
    .engine_version = hl_engine_version,
    .leak_check_nonvacuity = hl_c_backend_leak_check_nonvacuity,
    .checkpoint_broker_pair = hl_c_backend_checkpoint_broker_pair,
    .checkpoint_broker_accept = hl_c_backend_checkpoint_broker_accept,
    .checkpoint_trigger_create = hl_c_backend_checkpoint_trigger_create,
    .checkpoint_trigger_bump = hl_c_backend_checkpoint_trigger_bump,
    .checkpoint_trigger_destroy = hl_c_backend_checkpoint_trigger_destroy,
    .checkpoint_adopt = hl_c_backend_checkpoint_adopt,
    .checkpoint_interrupt_signal = hl_c_backend_checkpoint_interrupt_signal,
    .checkpoint_configure = hl_c_backend_checkpoint_configure,
    .create = hl_c_backend_create,
    .run = hl_c_backend_run,
    .request = hl_c_backend_request,
    .exit = hl_c_backend_exit,
    .destroy = hl_c_backend_destroy,
};

const hl_c_bridge_api *hl_c_bridge_api_v1(void) {
    return &HL_C_BRIDGE_API;
}
