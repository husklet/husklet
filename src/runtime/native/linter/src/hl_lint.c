#include <stdio.h>

#include "analyzers.h"
#include "cli.h"
#include "lint.h"
#include "policy.h"
#include "sources.h"

int main(int argc, char **argv) {
    LintConfig cfg;
    hl_lint_config_init(&cfg);
    HlLintCliResult cli_result = hl_lint_cli_parse(&cfg, argc, argv);
    if (cli_result != HL_LINT_CLI_RUN) {
        hl_lint_config_destroy(&cfg);
        return cli_result == HL_LINT_CLI_EXIT_SUCCESS ? 0 : 2;
    }

    StringList all_files;
    hl_lint_sources_collect(&cfg, &all_files);

    LintStats stats = {0, 0};
    if (all_files.count == 0) {
        fprintf(stdout, "%s: no source files matched\n", cfg.strict ? "error" : "warn");
        if (cfg.strict)
            stats.errors++;
        else
            stats.warnings++;
    }

    if (cfg.allow_getenv_files.count == 0) {
        // Engine currently centralizes env-var reads in environment.c.
        hl_lint_list_append(&cfg.allow_getenv_files, "src/core/environment.c");
    }

    const StringList *clang_tidy_files = cfg.clang_tidy_files.count > 0 ? &cfg.clang_tidy_files : &all_files;
    int rc = hl_lint_analyzers_run(&cfg, &all_files, clang_tidy_files, &stats);
    if (cfg.run_custom) hl_lint_policy_run(&cfg, &all_files, &stats);

    if (stats.errors == 0 && !cfg.strict && stats.warnings > 0) {
        fprintf(stdout, "hl-lint: warnings=%ld (non-fatal)\n", stats.warnings);
        rc = 0;
    } else {
        fprintf(stdout, "hl-lint: warnings=%ld errors=%ld\n", stats.warnings, stats.errors);
        if (stats.errors > 0)
            rc = 1;
        else if (cfg.strict && stats.warnings > 0)
            rc = 1;
    }

    if (cfg.strict) { fprintf(stdout, "hl-lint: strict mode enabled\n"); }

    hl_lint_list_destroy(&all_files);
    hl_lint_config_destroy(&cfg);
    return rc;
}
