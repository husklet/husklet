#ifndef HL_LINTER_CLI_H
#define HL_LINTER_CLI_H

#include "lint.h"

typedef enum {
    HL_LINT_CLI_RUN,
    HL_LINT_CLI_EXIT_SUCCESS,
    HL_LINT_CLI_EXIT_ERROR,
} HlLintCliResult;

void hl_lint_list_init(StringList *list);
void hl_lint_list_append(StringList *list, const char *value);
void hl_lint_list_destroy(StringList *list);

void hl_lint_config_init(LintConfig *config);
void hl_lint_config_destroy(LintConfig *config);
HlLintCliResult hl_lint_cli_parse(LintConfig *config, int argc, char **argv);

#endif
