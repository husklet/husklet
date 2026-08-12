#include "process.h"

#include <errno.h>
#include <stdio.h>
#include <string.h>
#ifdef _WIN32
#include <windows.h>
#endif

static int failures;

static void expect(bool condition, const char *message) {
    if (!condition) {
        fprintf(stderr, "FAIL: %s\n", message);
        failures++;
    }
}

static void test_capture_and_exit(const char *helper) {
    const char *const argv[] = {helper, "capture", NULL};
    HlLintProcessResult result;
    expect(hl_lint_process_run(argv, 0, &result) == 0, "capture helper spawned");
    expect(result.exit_code == 37, "nonzero exit code preserved");
    expect(result.term_signal == 0, "normal exit has no signal");
    expect(result.output && strstr(result.output, "helper-stdout"), "stdout captured");
    expect(result.output && strstr(result.output, "helper-stderr"), "stderr captured");
    hl_lint_process_result_destroy(&result);
}

static void test_argument_roundtrip(const char *helper) {
    const char *const argv[] = {helper,        "arguments",     "",           "two words",
                                "apostrophe'", "double\"quote", "trailing\\", NULL};
    HlLintProcessResult result;
    expect(hl_lint_process_run(argv, 0, &result) == 0, "argument helper spawned");
    expect(result.exit_code == 0, "argument helper succeeded");
    expect(result.output && strstr(result.output, "0:0:\n"), "empty argument preserved");
    expect(result.output && strstr(result.output, "1:9:two words\n"), "spaced argument preserved");
    expect(result.output && strstr(result.output, "2:11:apostrophe'\n"), "apostrophe argument preserved");
    expect(result.output && strstr(result.output, "3:12:double\"quote\n"), "double quote argument preserved");
    expect(result.output && strstr(result.output, "4:9:trailing\\\n"), "trailing backslash preserved");
    hl_lint_process_result_destroy(&result);
}

static void test_output_limit(const char *helper) {
    const char *const argv[] = {helper, "large", NULL};
    HlLintProcessResult result;
    expect(hl_lint_process_run(argv, 31, &result) == 0, "large-output helper spawned");
    expect(result.exit_code == 0, "large-output helper succeeded");
    expect(result.output_size == 31, "output limit enforced exactly");
    expect(result.output_truncated, "truncation reported");
    expect(result.output && result.output[31] == '\0', "truncated output remains terminated");
    hl_lint_process_result_destroy(&result);
}

static void test_missing_executable(void) {
    const char *const argv[] = {"/definitely/not/a/real/hl-lint-executable", NULL};
    HlLintProcessResult result;
    expect(hl_lint_process_run(argv, 0, &result) < 0, "missing executable is a spawn failure");
#ifdef _WIN32
    expect(result.platform_error == ERROR_FILE_NOT_FOUND || result.platform_error == ERROR_PATH_NOT_FOUND,
           "missing executable preserves the Windows error");
#else
    expect(result.platform_error == ENOENT, "missing executable preserves ENOENT");
#endif
    expect(result.exit_code == -1, "spawn failure is distinct from child exit");
    hl_lint_process_result_destroy(&result);
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: %s <helper>\n", argv[0]);
        return 2;
    }

    test_capture_and_exit(argv[1]);
    test_argument_roundtrip(argv[1]);
    test_output_limit(argv[1]);
    test_missing_executable();
    return failures ? 1 : 0;
}
