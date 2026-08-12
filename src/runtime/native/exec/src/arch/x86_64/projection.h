#ifndef HL_NATIVE_X86_64_PROJECTION_H
#define HL_NATIVE_X86_64_PROJECTION_H

#include "../../../include/executor.h"

#define HL_X86_PROJECTION_MAX_VIEWS 256u
#define HL_X86_DIRTY_CAPACITY 16u

int hl_x86_projection_validate(const hl_native_projection *);
int hl_x86_projection_resolve(const hl_native_projection *,
                              hl_native_x86_64_cpu *, uint64_t, uint64_t,
                              uint32_t);
int hl_x86_projection_written(hl_native_x86_64_cpu *, uint64_t, uint64_t);
int hl_x86_projection_switch_writable(const hl_native_x86_64_cpu *, uint64_t,
                                      uint64_t);

#endif
