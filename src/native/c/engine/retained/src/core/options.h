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

int hl_options_init(hl_options *options);
int hl_options_clone(hl_options *destination, const hl_options *source);
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

/* Existing engine internals resolve through the scoped store. */
const char *hl_option_get(const char *name);
int hl_option_set(const char *name, const char *value, int overwrite);
int hl_option_unset(const char *name);
void hl_option_reset(void);

#endif
