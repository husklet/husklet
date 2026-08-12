#ifndef HL_NATIVE_X86_64_RUN_H
#define HL_NATIVE_X86_64_RUN_H

#include "../../executor.h"

hl_native_status hl_native_x86_64_run(hl_native_executor *,
                                      hl_native_x86_64_cpu *,
                                      const hl_native_run_request *,
                                      hl_native_exit *);

#endif
