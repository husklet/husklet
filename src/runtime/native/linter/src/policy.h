#ifndef HL_LINTER_POLICY_H
#define HL_LINTER_POLICY_H

#include "lint.h"

void hl_lint_policy_run(const LintConfig *config, const StringList *files, LintStats *stats);

#endif
