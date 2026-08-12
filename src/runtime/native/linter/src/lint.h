#ifndef HL_LINTER_LINT_H
#define HL_LINTER_LINT_H

#include <stdbool.h>
#include <stddef.h>

typedef struct {
    char **items;
    size_t count;
    size_t cap;
} StringList;

typedef struct {
    StringList source_files;
    StringList source_dirs;
    StringList clang_tidy_files;
    StringList include_dirs;
    const char *clang_format_bin;
    const char *clang_tidy_bin;
    const char *cppcheck_bin;
    const char *compile_db_dir;
    const char *clang_tidy_checks;
    StringList allow_getenv_files;
    StringList allow_stdio_files;
    StringList allow_shell_files;
    int max_function_lines;
    int max_nesting_depth;
    int max_line_length;
    bool run_clang_format;
    bool run_clang_tidy;
    bool run_cppcheck;
    bool run_custom;
    bool strict;
} LintConfig;

typedef struct {
    long warnings;
    long errors;
} LintStats;

#endif
