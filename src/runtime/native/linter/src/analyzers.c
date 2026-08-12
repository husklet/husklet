#define _POSIX_C_SOURCE 200809L

#include "analyzers.h"

#include "process.h"

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifndef _WIN32
#include <unistd.h>
#else
#include <io.h>
#endif

static char *xdup_format(const char *format, ...) {
    va_list arguments;
    va_start(arguments, format);
    int length = vsnprintf(NULL, 0, format, arguments);
    va_end(arguments);
    if (length < 0) return NULL;
    char *result = malloc((size_t)length + 1);
    if (result == NULL) return NULL;
    va_start(arguments, format);
    vsnprintf(result, (size_t)length + 1, format, arguments);
    va_end(arguments);
    return result;
}

static bool has_ext(const char *path, const char *extension) {
    size_t path_length = strlen(path);
    size_t extension_length = strlen(extension);
    return path_length > extension_length && strcmp(path + path_length - extension_length, extension) == 0;
}

static void emit_diag(const char *severity, const char *path, int line, int col, const char *rule,
                      const char *message) {
    if (line > 0) {
        fprintf(stdout, "%s:%d:%d: [%s] %s: %s\n", path, line, col, severity, rule, message);
    } else {
        fprintf(stdout, "%s: [%s] %s: %s\n", path, severity, rule, message);
    }
}

static int run_command_argv(const char *label, const char *const argv[], bool strict, LintStats *stats) {
    HlLintProcessResult result;
    if (hl_lint_process_run(argv, 0, &result) != 0) {
#ifdef _WIN32
        fprintf(stdout, "error: %s failed to execute (Windows error %d)\n", label, result.platform_error);
#else
        fprintf(stdout, "error: %s failed to execute: %s\n", label, strerror(result.platform_error));
#endif
        stats->errors++;
        return 1;
    }
    int rc = result.exit_code;
    if (result.output_size > 0) { fwrite(result.output, 1, result.output_size, stdout); }
    if (result.output_truncated) {
        fprintf(stdout, "%s: %s output truncated\n", strict ? "error" : "warn", label);
        if (strict) {
            stats->errors++;
            rc = 1;
        } else {
            stats->warnings++;
        }
    }
    if (result.term_signal != 0) {
        fprintf(stdout, "error: %s terminated by signal %d\n", label, result.term_signal);
        stats->errors++;
        rc = 1;
    }
    hl_lint_process_result_destroy(&result);
    return rc;
}

static int run_clang_format(const LintConfig *cfg, const StringList *files, LintStats *stats) {
    int rc = 0;
    if (!cfg->run_clang_format) return 0;
    if (!cfg->clang_format_bin) {
        if (cfg->strict) {
            fprintf(stdout, "error: clang-format not configured\n");
            stats->errors++;
            return 1;
        }
        fprintf(stdout, "warn: skipping clang-format (binary not configured)\n");
        return 0;
    }

    for (size_t i = 0; i < files->count; i++) {
        const char *file = files->items[i];
        const char *const argv[] = {cfg->clang_format_bin, "--dry-run", "--Werror", "--style=file",
                                    "--ferror-limit=1",    file,        NULL};
        int c = run_command_argv("clang-format", argv, cfg->strict, stats);
        if (c != 0) {
            if (cfg->strict) {
                emit_diag("error", file, 0, 0, "clang-format", "formatting violation");
                stats->errors++;
                rc = 1;
            } else {
                emit_diag("warn", file, 0, 0, "clang-format", "formatting violation");
                stats->warnings++;
            }
        }
        if (cfg->strict && rc != 0) return 1;
        if (cfg->strict)
            rc = (rc != 0) ? rc : c;
        else
            rc = 0;
    }
    return rc;
}

static int run_clang_tidy(const LintConfig *cfg, const StringList *files, LintStats *stats) {
    int rc = 0;
    if (!cfg->run_clang_tidy) return 0;
    if (!cfg->clang_tidy_bin) {
        if (cfg->strict) {
            fprintf(stdout, "error: clang-tidy not configured\n");
            stats->errors++;
            return 1;
        }
        fprintf(stdout, "warn: skipping clang-tidy (binary not configured)\n");
        return 0;
    }
    char *compile_db = cfg->compile_db_dir ? xdup_format("%s/compile_commands.json", cfg->compile_db_dir) : NULL;
    if (!compile_db ||
#ifdef _WIN32
        _access(compile_db, 0) != 0) {
#else
        access(compile_db, F_OK) != 0) {
#endif
        if (cfg->strict) {
            fprintf(stdout, "error: compile database missing for clang-tidy: %s\n",
                    compile_db ? compile_db : "<unset>");
            stats->errors++;
            free(compile_db);
            return 1;
        }
        fprintf(stdout, "warn: skipping clang-tidy (missing compile db)\n");
        free(compile_db);
        return 0;
    }
    free(compile_db);

    for (size_t i = 0; i < files->count; i++) {
        const char *file = files->items[i];
        if (!has_ext(file, ".c")) continue;
        char *checks = xdup_format("--checks=%s", cfg->clang_tidy_checks ? cfg->clang_tidy_checks
                                                                         : "bugprone-*,clang-analyzer-*,performance-*");
        if (!checks) {
            fprintf(stdout, "error: out of memory building clang-tidy command\n");
            return 1;
        }
        const char *const argv[] = {cfg->clang_tidy_bin,      "--quiet", "-p",
                                    cfg->compile_db_dir,      checks,    "--extra-arg=-std=c11",
                                    "--warnings-as-errors=*", file,      NULL};
        int c = run_command_argv("clang-tidy", argv, cfg->strict, stats);
        free(checks);
        if (c != 0) {
            emit_diag("warn", file, 0, 0, "clang-tidy", "diagnostic(s) reported");
            if (cfg->strict) {
                stats->errors++;
                rc = 1;
                continue;
            }
            stats->warnings++;
            c = 0;
        }
        rc = (rc != 0) ? rc : c;
    }
    return rc;
}

static int run_cppcheck(const LintConfig *cfg, const StringList *files, LintStats *stats) {
    (void)files;
    if (!cfg->run_cppcheck) return 0;
    if (!cfg->cppcheck_bin) {
        if (cfg->strict) {
            fprintf(stdout, "error: cppcheck not configured\n");
            stats->errors++;
            return 1;
        }
        fprintf(stdout, "warn: skipping cppcheck (binary not configured)\n");
        return 0;
    }

    char *project = cfg->compile_db_dir ? xdup_format("--project=%s/compile_commands.json", cfg->compile_db_dir) : NULL;
    if (!project) {
        fprintf(stdout, "error: cppcheck requires a compile commands directory\n");
        stats->errors++;
        return 1;
    }
    const char *argv[] = {
        cfg->cppcheck_bin,
        "--quiet",
        "--std=c11",
        "--enable=warning,performance,portability",
        "--inconclusive",
        "--suppress=missingIncludeSystem",
        "--suppress=unmatchedSuppression",
        "--suppress=unusedStructMember",
        "--suppress=constParameter",
        "--suppress=normalCheckLevelMaxBranches",
        "--suppress=toomanyconfigs",
        "--suppress=preprocessorErrorDirective",
        "--error-exitcode=1",
        project,
        NULL,
    };
    int rc = run_command_argv("cppcheck", argv, cfg->strict, stats);
    free(project);
    if (rc == 0) return 0;
    emit_diag("warn", cfg->compile_db_dir, 0, 0, "cppcheck", "diagnostic(s) reported");
    if (cfg->strict) {
        stats->errors++;
        return 1;
    }
    stats->warnings++;
    return 0;
}

int hl_lint_analyzers_run(const LintConfig *config, const StringList *files, const StringList *clang_tidy_files,
                          LintStats *stats) {
    int result = 0;
    if (config->run_clang_format) result = run_clang_format(config, files, stats);
    if (result == 0 && config->run_clang_tidy) result = run_clang_tidy(config, clang_tidy_files, stats);
    if (result == 0 && config->run_cppcheck) result = run_cppcheck(config, files, stats);
    return result;
}
