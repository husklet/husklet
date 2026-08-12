#include "cli.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

void hl_lint_list_init(StringList *list) {
    list->items = NULL;
    list->count = 0;
    list->cap = 0;
}

static char *string_duplicate(const char *value) {
    size_t size = strlen(value) + 1;
    char *copy = malloc(size);
    if (copy != NULL) memcpy(copy, value, size);
    return copy;
}

static bool list_contains(const StringList *list, const char *value) {
    for (size_t i = 0; i < list->count; i++) {
        if (strcmp(list->items[i], value) == 0) return true;
    }
    return false;
}

void hl_lint_list_append(StringList *list, const char *value) {
    if (list_contains(list, value)) return;
    if (list->count + 1 > list->cap) {
        size_t capacity = list->cap == 0 ? 64 : list->cap * 2;
        char **items = realloc(list->items, capacity * sizeof(*items));
        if (items == NULL) {
            fprintf(stdout, "error: out of memory while collecting paths\n");
            exit(1);
        }
        list->items = items;
        list->cap = capacity;
    }
    list->items[list->count] = string_duplicate(value);
    if (list->items[list->count] == NULL) {
        fprintf(stdout, "error: out of memory while collecting paths\n");
        exit(1);
    }
    list->count++;
}

void hl_lint_list_destroy(StringList *list) {
    if (list == NULL || list->items == NULL) return;
    for (size_t i = 0; i < list->count; i++) {
        free(list->items[i]);
    }
    free(list->items);
    hl_lint_list_init(list);
}

void hl_lint_config_init(LintConfig *config) {
    *config = (LintConfig){
        .run_clang_format = true,
        .run_clang_tidy = true,
        .run_cppcheck = true,
        .run_custom = true,
        .clang_tidy_checks = "clang-analyzer-*,-clang-analyzer-security.insecureAPI.DeprecatedOrUnsafeBufferHandling,"
                             "bugprone-assignment-in-if-condition,bugprone-branch-clone,bugprone-inc-dec-in-conditions,"
                             "bugprone-infinite-loop,bugprone-not-null-terminated-result,bugprone-posix-return,"
                             "bugprone-signal-handler,bugprone-sizeof-expression,bugprone-suspicious-memory-comparison,"
                             "bugprone-suspicious-memset-usage,bugprone-undefined-memory-manipulation",
    };
    hl_lint_list_init(&config->source_files);
    hl_lint_list_init(&config->source_dirs);
    hl_lint_list_init(&config->clang_tidy_files);
    hl_lint_list_init(&config->include_dirs);
    hl_lint_list_init(&config->allow_getenv_files);
    hl_lint_list_init(&config->allow_stdio_files);
    hl_lint_list_init(&config->allow_shell_files);
}

void hl_lint_config_destroy(LintConfig *config) {
    hl_lint_list_destroy(&config->source_files);
    hl_lint_list_destroy(&config->source_dirs);
    hl_lint_list_destroy(&config->clang_tidy_files);
    hl_lint_list_destroy(&config->include_dirs);
    hl_lint_list_destroy(&config->allow_getenv_files);
    hl_lint_list_destroy(&config->allow_stdio_files);
    hl_lint_list_destroy(&config->allow_shell_files);
}

static void print_usage(const char *program) {
    fprintf(stdout, "usage: %s [options] [--source-dir <path>]... [--source-file <path>]...\n", program);
    fprintf(stdout, "options:\n");
    fprintf(stdout, "  --source-dir PATH         add recursive source directory (default: src)\n");
    fprintf(stdout, "  --source-file PATH        add explicit source file\n");
    fprintf(stdout, "  --clang-tidy-source-file PATH analyze a compiled translation unit\n");
    fprintf(stdout, "  --include-dir PATH        add include directory for cppcheck\n");
    fprintf(stdout, "  --compile-commands-dir DIR directory containing compile_commands.json for clang-tidy\n");
    fprintf(stdout, "  --clang-format-bin PATH   clang-format path\n");
    fprintf(stdout, "  --clang-tidy-bin PATH     clang-tidy path\n");
    fprintf(stdout, "  --cppcheck-bin PATH       cppcheck path\n");
    fprintf(stdout,
            "  --clang-tidy-checks LIST  clang-tidy checks (default: bugprone-*,clang-analyzer-*,performance-*)\n");
    fprintf(stdout, "  --max-function-lines N    opt in to lexical function-length warnings\n");
    fprintf(stdout, "  --max-nesting N           opt in to lexical brace-depth warnings\n");
    fprintf(stdout, "  --max-line-length N       opt in to line-length warnings\n");
    fprintf(stdout, "  --strict                  fail on warnings as errors\n");
    fprintf(stdout, "  --skip-clang-format       disable clang-format stage\n");
    fprintf(stdout, "  --skip-clang-tidy         disable clang-tidy stage\n");
    fprintf(stdout, "  --skip-cppcheck           disable cppcheck stage\n");
    fprintf(stdout, "  --skip-custom             disable custom heuristics stage\n");
    fprintf(stdout, "  --allow-getenv-file PATH  allow direct environment access in this source file\n");
    fprintf(stdout, "  --allow-stdio-file PATH   temporarily allow direct console output in this file\n");
    fprintf(stdout, "  --allow-shell-file PATH   temporarily allow shell execution in this file\n");
    fprintf(stdout, "  --clang-format-check/--clang-format-no-check\n");
    fprintf(stdout, "  --clang-tidy-check/--clang-tidy-no-check\n");
    fprintf(stdout, "  --cppcheck-check/--cppcheck-no-check\n");
    fprintf(stdout, "  --help                    show this help\n");
}

static bool parse_integer_option(const char *option, const char *value, int *result) {
    char *end = NULL;
    long parsed = strtol(value, &end, 10);
    if (end == NULL || *end != '\0' || parsed < 0 || parsed > 1000000) {
        fprintf(stdout, "error: invalid integer `%s` for %s\n", value, option);
        return false;
    }
    *result = (int)parsed;
    return true;
}

static const char *option_value(int *index, int argc, char **argv) {
    if (*index + 1 < argc) return argv[++*index];
    fprintf(stdout, "error: %s expects a value\n", argv[*index]);
    return NULL;
}

HlLintCliResult hl_lint_cli_parse(LintConfig *config, int argc, char **argv) {
    for (int index = 1; index < argc; index++) {
        const char *argument = argv[index];
        const char *value;
        if (strcmp(argument, "--help") == 0 || strcmp(argument, "-h") == 0) {
            print_usage(argv[0]);
            return HL_LINT_CLI_EXIT_SUCCESS;
        }
        if (strcmp(argument, "--source-dir") == 0 || strcmp(argument, "--src") == 0) {
            if ((value = option_value(&index, argc, argv)) == NULL) return HL_LINT_CLI_EXIT_ERROR;
            hl_lint_list_append(&config->source_dirs, value);
        } else if (strcmp(argument, "--source-file") == 0 || strcmp(argument, "--file") == 0) {
            if ((value = option_value(&index, argc, argv)) == NULL) return HL_LINT_CLI_EXIT_ERROR;
            hl_lint_list_append(&config->source_files, value);
        } else if (strcmp(argument, "--clang-tidy-source-file") == 0) {
            if ((value = option_value(&index, argc, argv)) == NULL) return HL_LINT_CLI_EXIT_ERROR;
            hl_lint_list_append(&config->clang_tidy_files, value);
        } else if (strcmp(argument, "--include-dir") == 0 || strcmp(argument, "-I") == 0) {
            if ((value = option_value(&index, argc, argv)) == NULL) return HL_LINT_CLI_EXIT_ERROR;
            hl_lint_list_append(&config->include_dirs, value);
        } else if (strcmp(argument, "--compile-commands-dir") == 0) {
            if ((config->compile_db_dir = option_value(&index, argc, argv)) == NULL) return HL_LINT_CLI_EXIT_ERROR;
        } else if (strcmp(argument, "--clang-format-bin") == 0) {
            if ((config->clang_format_bin = option_value(&index, argc, argv)) == NULL) return HL_LINT_CLI_EXIT_ERROR;
        } else if (strcmp(argument, "--clang-tidy-bin") == 0) {
            if ((config->clang_tidy_bin = option_value(&index, argc, argv)) == NULL) return HL_LINT_CLI_EXIT_ERROR;
        } else if (strcmp(argument, "--cppcheck-bin") == 0) {
            if ((config->cppcheck_bin = option_value(&index, argc, argv)) == NULL) return HL_LINT_CLI_EXIT_ERROR;
        } else if (strcmp(argument, "--clang-tidy-checks") == 0) {
            if ((config->clang_tidy_checks = option_value(&index, argc, argv)) == NULL) return HL_LINT_CLI_EXIT_ERROR;
        } else if (strcmp(argument, "--max-function-lines") == 0) {
            if ((value = option_value(&index, argc, argv)) == NULL ||
                !parse_integer_option(argument, value, &config->max_function_lines))
                return HL_LINT_CLI_EXIT_ERROR;
        } else if (strcmp(argument, "--max-nesting") == 0) {
            if ((value = option_value(&index, argc, argv)) == NULL ||
                !parse_integer_option(argument, value, &config->max_nesting_depth))
                return HL_LINT_CLI_EXIT_ERROR;
        } else if (strcmp(argument, "--max-line-length") == 0) {
            if ((value = option_value(&index, argc, argv)) == NULL ||
                !parse_integer_option(argument, value, &config->max_line_length))
                return HL_LINT_CLI_EXIT_ERROR;
        } else if (strcmp(argument, "--strict") == 0) {
            config->strict = true;
        } else if (strcmp(argument, "--skip-clang-format") == 0 || strcmp(argument, "--clang-format-no-check") == 0) {
            config->run_clang_format = false;
        } else if (strcmp(argument, "--skip-clang-tidy") == 0 || strcmp(argument, "--clang-tidy-no-check") == 0) {
            config->run_clang_tidy = false;
        } else if (strcmp(argument, "--skip-cppcheck") == 0 || strcmp(argument, "--cppcheck-no-check") == 0) {
            config->run_cppcheck = false;
        } else if (strcmp(argument, "--skip-custom") == 0) {
            config->run_custom = false;
        } else if (strcmp(argument, "--allow-getenv-file") == 0) {
            if ((value = option_value(&index, argc, argv)) == NULL) return HL_LINT_CLI_EXIT_ERROR;
            hl_lint_list_append(&config->allow_getenv_files, value);
        } else if (strcmp(argument, "--allow-stdio-file") == 0) {
            if ((value = option_value(&index, argc, argv)) == NULL) return HL_LINT_CLI_EXIT_ERROR;
            hl_lint_list_append(&config->allow_stdio_files, value);
        } else if (strcmp(argument, "--allow-shell-file") == 0) {
            if ((value = option_value(&index, argc, argv)) == NULL) return HL_LINT_CLI_EXIT_ERROR;
            hl_lint_list_append(&config->allow_shell_files, value);
        } else if (strcmp(argument, "--clang-format-check") == 0) {
            config->run_clang_format = true;
        } else if (strcmp(argument, "--clang-tidy-check") == 0) {
            config->run_clang_tidy = true;
        } else if (strcmp(argument, "--cppcheck-check") == 0) {
            config->run_cppcheck = true;
        } else {
            fprintf(stdout, "error: unknown option `%s`\n", argument);
            print_usage(argv[0]);
            return HL_LINT_CLI_EXIT_ERROR;
        }
    }
    return HL_LINT_CLI_RUN;
}

