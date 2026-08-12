#ifndef HL_LINTER_ANALYZERS_H
#define HL_LINTER_ANALYZERS_H

#include "lint.h"

int hl_lint_analyzers_run(const LintConfig *config, const StringList *files, const StringList *clang_tidy_files,
                          LintStats *stats);

#endif
