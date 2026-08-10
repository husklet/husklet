#ifndef HL_C_MAIN_IMAGE_PLAN_H
#define HL_C_MAIN_IMAGE_PLAN_H
#include <stdint.h>
#define HL_C_MAIN_IMAGE_PLAN_ABI 1u
#define HL_C_IMAGE_EXECUTABLE 1u
#define HL_C_IMAGE_POSITION_INDEPENDENT 2u
typedef struct hl_c_main_image_plan {
    uint32_t abi, size, architecture, kind;
    uint64_t link_start, link_end;
    uint32_t has_interpreter, reserved;
    uint64_t interpreter_identity;
} hl_c_main_image_plan;
#endif
