#ifndef HL_CORE_ENVIRONMENT_H
#define HL_CORE_ENVIRONMENT_H

/*
 * Sole production boundary for ambient host environment variables.
 * Launch/runtime configuration belongs in hl_options; only process bootstrap
 * inputs and the debug-log seed may enter through this module.
 */
const char *hl_environment_debug_log(void);

/* Returns 1 for a consumed descriptor, 0 when absent, and -1 when malformed. */
int hl_environment_take_activation_descriptor(long *descriptor);

#endif
