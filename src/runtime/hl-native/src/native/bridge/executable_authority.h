#ifndef HL_C_BACKEND_EXECUTABLE_AUTHORITY_H
#define HL_C_BACKEND_EXECUTABLE_AUTHORITY_H

#include "hl/engine.h"

HL_EXTERN_C_BEGIN

/*
 * Opens the workspace-staged executable in the host namespace and prepares the
 * authority consumed by hl_engine_create.  The returned handle transfers only
 * when hl_engine_create succeeds; call discard on every failure path.
 */
HL_API hl_status hl_c_backend_executable_open(const hl_host_services *services, const char *host_path,
                                              hl_engine_executable *output);

/* Releases an authority that was not transferred to a successfully-created
 * engine. */
HL_API void hl_c_backend_executable_discard(const hl_host_services *services, hl_engine_executable *executable);

HL_EXTERN_C_END

#endif
