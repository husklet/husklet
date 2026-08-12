#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>

static bool has_arg(int argc, char **argv, const char *expected) {
    for (int i = 1; i < argc; ++i) {
        if (strcmp(argv[i], expected) == 0) { return true; }
    }
    return false;
}

static const char *value_after(int argc, char **argv, const char *option) {
    for (int i = 1; i + 1 < argc; ++i) {
        if (strcmp(argv[i], option) == 0) { return argv[i + 1]; }
    }
    return NULL;
}

static const char *value_with_prefix(int argc, char **argv, const char *prefix) {
    size_t length = strlen(prefix);
    for (int i = 1; i < argc; ++i) {
        if (strncmp(argv[i], prefix, length) == 0) return argv[i] + length;
    }
    return NULL;
}

static bool is_directory(const char *path) {
    struct stat info;
    return path && stat(path, &info) == 0 && S_ISDIR(info.st_mode);
}

static bool is_regular_file(const char *path) {
    struct stat info;
    return path && stat(path, &info) == 0 && S_ISREG(info.st_mode);
}

static int check_clang_tidy(int argc, char **argv) {
    const char *compile_dir = value_after(argc, argv, "-p");
    if (!is_directory(compile_dir)) {
        fputs("fake-analyzer: clang-tidy -p is not a directory\n", stderr);
        return 2;
    }

    char database[4096];
    int length = snprintf(database, sizeof database, "%s/compile_commands.json", compile_dir);
    if (length < 0 || (size_t)length >= sizeof database || !is_regular_file(database)) {
        fputs("fake-analyzer: clang-tidy compile database missing\n", stderr);
        return 2;
    }
    if (!has_arg(argc, argv, "--extra-arg=-std=c11") || !has_arg(argc, argv, "--checks=bugprone-*,performance-*") ||
        !has_arg(argc, argv, "--warnings-as-errors=*")) {
        fputs("fake-analyzer: clang-tidy required argument missing\n", stderr);
        return 2;
    }

    puts("fake-analyzer: clang-tidy argv ok");
    return 0;
}

static int check_cppcheck(int argc, char **argv) {
    const char *project = value_with_prefix(argc, argv, "--project=");
    if (!is_regular_file(project)) {
        fputs("fake-analyzer: cppcheck compile database missing\n", stderr);
        return 2;
    }
    if (!has_arg(argc, argv, "--std=c11") || !has_arg(argc, argv, "--error-exitcode=1") ||
        !has_arg(argc, argv, "--suppress=unmatchedSuppression") ||
        !has_arg(argc, argv, "--suppress=unusedStructMember") || !has_arg(argc, argv, "--suppress=constParameter") ||
        !has_arg(argc, argv, "--suppress=normalCheckLevelMaxBranches") ||
        !has_arg(argc, argv, "--suppress=toomanyconfigs") ||
        !has_arg(argc, argv, "--suppress=preprocessorErrorDirective") || has_arg(argc, argv, "2>&1")) {
        fputs("fake-analyzer: cppcheck argument corruption\n", stderr);
        return 2;
    }
    puts("fake-analyzer: cppcheck argv ok");
    return 0;
}

int main(int argc, char **argv) {
    if (has_arg(argc, argv, "--quiet") && has_arg(argc, argv, "-p")) { return check_clang_tidy(argc, argv); }
    if (has_arg(argc, argv, "--enable=warning,performance,portability")) { return check_cppcheck(argc, argv); }

    fputs("fake-analyzer: unknown invocation\n", stderr);
    return 2;
}
