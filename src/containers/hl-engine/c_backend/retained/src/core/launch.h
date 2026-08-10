#ifndef HL_CORE_LAUNCH_H
#define HL_CORE_LAUNCH_H

#include <stdint.h>
#include "options.h"

typedef int (*hl_launch_runner)(const char *rootfs, const char *executable_host, uint32_t argc, char *const argv[],
                                const hl_options *options, const char *result_path);
int hl_run_config_file_with(const char *path, hl_launch_runner runner);

#endif
