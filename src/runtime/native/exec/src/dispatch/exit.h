#ifndef HL_NATIVE_DISPATCH_EXIT_H
#define HL_NATIVE_DISPATCH_EXIT_H

#include "../../include/executor.h"

/* Constructs the architecture-neutral record only after architectural state
 * is fully spilled. Architecture-private helper reasons must be resolved
 * before this boundary. */
hl_native_status hl_native_exit_build(hl_native_exit *, uint32_t, uint32_t, uint64_t, uint64_t, uint64_t, uint64_t);

#endif
