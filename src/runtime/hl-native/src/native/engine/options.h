#ifndef HL_CORE_OPTIONS_H
#define HL_CORE_OPTIONS_H

#include <stddef.h>

/*
 * An option store is owned by one launch/engine.  The definition table remains
 * process-wide and immutable; all values and accounting live in this object.
 */
typedef struct hl_options {
    char **values;
    size_t *value_sizes;
    size_t value_count;
    size_t store_size;
} hl_options;

/*
 * A successful exec publishes three related records: the process-private guest
 * environment and the launch-option escape/exactness flags.  Preparing owns
 * every allocation; committing only swaps already-owned stores and cannot fail.
 */
typedef struct hl_exec_environment_update {
    hl_options process;
    hl_options state;
    hl_options *process_target;
    hl_options *state_target;
    int separate_state;
    int prepared;
} hl_exec_environment_update;

int hl_options_init(hl_options *options);
/* Deep-copy one already-validated, unique record set into the C read view. */
int hl_options_init_records(hl_options *options, size_t count, const char *const *names, const char *const *values);
int hl_options_clone(hl_options *destination, const hl_options *source);
/* Validate a complete store before lending its lifetime to an engine. */
int hl_options_validate(const hl_options *options);
/* Snapshot the scoped, process, or centralized default context into an owned store. */
int hl_options_clone_current(hl_options *destination);
/* Import supported host environment defaults without replacing explicit values. */
void hl_options_import_environment(hl_options *options);
void hl_options_destroy(hl_options *options);
const char *hl_options_get(const hl_options *options, const char *name);
int hl_options_set(hl_options *options, const char *name, const char *value, int overwrite);
int hl_options_unset(hl_options *options, const char *name);

/* Bind an owned store to the calling execution context; returns the previous binding. */
hl_options *hl_options_bind(hl_options *options);
/* Production workers are process-isolated; this fallback is inherited by all of their threads. */
hl_options *hl_options_bind_process(hl_options *options);
/* Bind process-private runtime overrides. Missing values fall back to launch options. */
hl_options *hl_options_bind_process_state(hl_options *options);
const char *hl_process_guest_environment_get(void);
int hl_process_guest_environment_set(const char *value);
int hl_process_guest_environment_unset(void);
int hl_exec_environment_prepare(hl_exec_environment_update *update, const char *serialized);
void hl_exec_environment_commit(hl_exec_environment_update *update);
void hl_exec_environment_discard(hl_exec_environment_update *update);

/* Existing engine internals resolve through the scoped store. */
const char *hl_option_get(const char *name);
/* Read a flag value with an explicit missing-value default; "0" is false and
 * every other registered flag spelling is true. */
int hl_option_flag_value(const char *name, int missing_value);
int hl_option_set(const char *name, const char *value, int overwrite);
int hl_option_unset(const char *name);
void hl_option_reset(void);

#endif
