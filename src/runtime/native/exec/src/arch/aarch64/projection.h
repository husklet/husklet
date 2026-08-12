#ifndef HL_NATIVE_AARCH64_PROJECTION_H
#define HL_NATIVE_AARCH64_PROJECTION_H

#include "../../../include/executor.h"

#include <stddef.h>
#include <stdint.h>

#define HL_A64_PROJECTION_MAX_VIEWS 256u
#define HL_A64_PERMISSION_READ 1u
#define HL_A64_PERMISSION_WRITE 2u
#define HL_A64_PERMISSION_EXECUTE 4u

typedef hl_native_projection_view hl_a64_view;
typedef hl_native_projection hl_a64_projection;

int hl_a64_projection_validate(const hl_a64_projection *);
int hl_a64_dirty_can_archive(const hl_native_aarch64_cpu *);
int hl_a64_projection_resolve(const hl_a64_projection *,
                              hl_native_aarch64_cpu *, uint64_t, uint64_t,
                              uint32_t);

#endif
