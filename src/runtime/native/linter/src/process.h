#ifndef HL_LINT_PROCESS_H
#define HL_LINT_PROCESS_H

#include <stdbool.h>
#include <stddef.h>

typedef struct {
    char *output;
    size_t output_size;
    int exit_code;
    int term_signal;
    int platform_error;
    bool output_truncated;
} HlLintProcessResult;

/*
 * Runs a NULL-terminated argv vector without a shell and captures merged
 * stdout/stderr. A zero return means the child was spawned and reaped; its
 * disposition is in exit_code/term_signal. A negative return is an
 * infrastructure failure and platform_error contains errno/GetLastError.
 */
int hl_lint_process_run(const char *const argv[], size_t output_limit, HlLintProcessResult *result);
void hl_lint_process_result_destroy(HlLintProcessResult *result);

#endif
