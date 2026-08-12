#define _POSIX_C_SOURCE 200809L

#include "sources.h"

#include "cli.h"

#include <errno.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#else
#include <dirent.h>
#endif

#ifdef _WIN32
static char *duplicate_format(const char *format, ...) {
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
#endif

static bool has_extension(const char *path, const char *extension) {
    size_t path_length = strlen(path);
    size_t extension_length = strlen(extension);
    return path_length > extension_length && strcmp(path + path_length - extension_length, extension) == 0;
}

static bool is_source_file(const char *path) {
    return has_extension(path, ".c") || has_extension(path, ".h") || has_extension(path, ".m") ||
           has_extension(path, ".mm");
}

static bool should_skip_directory(const char *name) {
    return strcmp(name, ".") == 0 || strcmp(name, "..") == 0 || name[0] == '.' || strcmp(name, "build") == 0 ||
           strncmp(name, "build-", 6) == 0 || strcmp(name, "result") == 0 || strncmp(name, "result-", 7) == 0 ||
           strcmp(name, "hl_errmat_") == 0;
}

#ifdef _WIN32
static wchar_t *path_to_wide(const char *path) {
    int count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, NULL, 0);
    if (count == 0) return NULL;
    wchar_t *wide = malloc((size_t)count * sizeof(*wide));
    if (wide == NULL) return NULL;
    if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, wide, count) == 0) {
        free(wide);
        return NULL;
    }
    return wide;
}

static char *path_from_wide(const wchar_t *path) {
    int count = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, path, -1, NULL, 0, NULL, NULL);
    if (count == 0) return NULL;
    char *utf8 = malloc((size_t)count);
    if (utf8 == NULL) return NULL;
    if (WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, path, -1, utf8, count, NULL, NULL) == 0) {
        free(utf8);
        return NULL;
    }
    return utf8;
}

static void collect_recursive(const char *root, StringList *files) {
    wchar_t *wide = path_to_wide(root);
    if (wide == NULL) {
        fprintf(stdout, "warn: invalid UTF-8 path `%s`\n", root);
        return;
    }
    DWORD attributes = GetFileAttributesW(wide);
    if (attributes == INVALID_FILE_ATTRIBUTES) {
        fprintf(stdout, "warn: cannot inspect path `%s` (Windows error %lu)\n", root, (unsigned long)GetLastError());
        free(wide);
        return;
    }
    if ((attributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
        size_t length = wcslen(wide);
        wchar_t *pattern = malloc((length + 3) * sizeof(*pattern));
        if (pattern == NULL) {
            free(wide);
            return;
        }
        memcpy(pattern, wide, length * sizeof(*pattern));
        pattern[length++] = L'\\';
        pattern[length++] = L'*';
        pattern[length] = L'\0';
        WIN32_FIND_DATAW entry;
        HANDLE search = FindFirstFileW(pattern, &entry);
        free(pattern);
        if (search == INVALID_HANDLE_VALUE) {
            fprintf(stdout, "warn: cannot open directory `%s` (Windows error %lu)\n", root,
                    (unsigned long)GetLastError());
            free(wide);
            return;
        }
        do {
            char *name = path_from_wide(entry.cFileName);
            if (name == NULL || should_skip_directory(name)) {
                free(name);
                continue;
            }
            char *child = duplicate_format("%s/%s", root, name);
            free(name);
            if (child == NULL) continue;
            if ((entry.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
                collect_recursive(child, files);
            } else if (is_source_file(child)) {
                hl_lint_list_append(files, child);
            }
            free(child);
        } while (FindNextFileW(search, &entry));
        FindClose(search);
    } else if (is_source_file(root)) {
        hl_lint_list_append(files, root);
    }
    free(wide);
}
#else
static void collect_recursive(const char *root, StringList *files) {
    struct stat status;
    if (stat(root, &status) != 0) {
        fprintf(stdout, "warn: cannot stat path `%s`: %s\n", root, strerror(errno));
        return;
    }
    if (S_ISDIR(status.st_mode)) {
        DIR *directory = opendir(root);
        if (directory == NULL) {
            fprintf(stdout, "warn: cannot open directory `%s`: %s\n", root, strerror(errno));
            return;
        }
        while (true) {
            struct dirent *entry = readdir(directory);
            if (entry == NULL) break;
            if (should_skip_directory(entry->d_name)) continue;
            char child[4096];
            snprintf(child, sizeof(child), "%s/%s", root, entry->d_name);
            if (stat(child, &status) != 0) continue;
            if (S_ISDIR(status.st_mode)) {
                collect_recursive(child, files);
            } else if (S_ISREG(status.st_mode) && is_source_file(child)) {
                hl_lint_list_append(files, child);
            }
        }
        closedir(directory);
    } else if (S_ISREG(status.st_mode) && is_source_file(root)) {
        hl_lint_list_append(files, root);
    }
}
#endif

void hl_lint_sources_collect(const LintConfig *config, StringList *files) {
    hl_lint_list_init(files);
    if (config->source_files.count == 0 && config->source_dirs.count == 0) {
        collect_recursive("src", files);
        return;
    }
    for (size_t index = 0; index < config->source_files.count; index++) {
        hl_lint_list_append(files, config->source_files.items[index]);
    }
    for (size_t index = 0; index < config->source_dirs.count; index++) {
        collect_recursive(config->source_dirs.items[index], files);
    }
}
