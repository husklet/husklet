#define WIN32_LEAN_AND_MEAN
#include <windows.h>

#include "process.h"

#include <errno.h>
#include <limits.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

enum { HL_LINT_PROCESS_DEFAULT_LIMIT = 8 * 1024 * 1024 };

static void result_init(HlLintProcessResult *result) {
    memset(result, 0, sizeof *result);
    result->exit_code = -1;
}

static int append_output(HlLintProcessResult *result, const char *data, size_t length, size_t limit, size_t *capacity) {
    size_t available = result->output_size < limit ? limit - result->output_size : 0;
    size_t keep = length < available ? length : available;
    size_t required;
    char *grown;
    if (keep < length) result->output_truncated = true;
    if (keep == 0) return 0;
    required = result->output_size + keep + 1;
    if (required > *capacity) {
        size_t next = *capacity ? *capacity : 4096;
        while (next < required) {
            if (next > SIZE_MAX / 2) return -1;
            next *= 2;
        }
        if (next > limit + 1) next = limit + 1;
        grown = realloc(result->output, next);
        if (grown == NULL) return -1;
        result->output = grown;
        *capacity = next;
    }
    memcpy(result->output + result->output_size, data, keep);
    result->output_size += keep;
    result->output[result->output_size] = '\0';
    return 0;
}

static wchar_t *utf8_to_wide(const char *text) {
    int count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text, -1, NULL, 0);
    wchar_t *wide;
    if (count == 0) return NULL;
    wide = malloc((size_t)count * sizeof(*wide));
    if (wide == NULL) return NULL;
    if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text, -1, wide, count) == 0) {
        free(wide);
        return NULL;
    }
    return wide;
}

static size_t quoted_size(const wchar_t *arg) {
    size_t size = 3;
    size_t slashes = 0;
    for (; *arg != L'\0'; ++arg) {
        if (*arg == L'\\') {
            ++slashes;
        } else {
            size += slashes + (*arg == L'"' ? slashes + 2 : 1);
            slashes = 0;
        }
    }
    return size + slashes * 2;
}

static wchar_t *quote_arg(wchar_t *out, const wchar_t *arg) {
    size_t slashes = 0;
    *out++ = L'"';
    for (; *arg != L'\0'; ++arg) {
        if (*arg == L'\\') {
            ++slashes;
            continue;
        }
        if (*arg == L'"') {
            while (slashes != 0) {
                *out++ = L'\\';
                *out++ = L'\\';
                --slashes;
            }
            *out++ = L'\\';
            *out++ = L'"';
        } else {
            while (slashes != 0) {
                *out++ = L'\\';
                --slashes;
            }
            *out++ = *arg;
        }
    }
    while (slashes != 0) {
        *out++ = L'\\';
        *out++ = L'\\';
        --slashes;
    }
    *out++ = L'"';
    return out;
}

static wchar_t *command_line(const char *const argv[]) {
    size_t count = 0;
    size_t total = 1;
    wchar_t **wide;
    wchar_t *line;
    wchar_t *cursor;
    while (argv[count] != NULL)
        ++count;
    wide = calloc(count, sizeof(*wide));
    if (wide == NULL) return NULL;
    for (size_t i = 0; i < count; ++i) {
        wide[i] = utf8_to_wide(argv[i]);
        if (wide[i] == NULL) goto fail;
        total += quoted_size(wide[i]) + 1;
    }
    line = malloc(total * sizeof(*line));
    if (line == NULL) goto fail;
    cursor = line;
    for (size_t i = 0; i < count; ++i) {
        if (i != 0) *cursor++ = L' ';
        cursor = quote_arg(cursor, wide[i]);
        free(wide[i]);
    }
    *cursor = L'\0';
    free(wide);
    return line;
fail:
    for (size_t i = 0; i < count; ++i)
        free(wide[i]);
    free(wide);
    return NULL;
}

int hl_lint_process_run(const char *const argv[], size_t output_limit, HlLintProcessResult *result) {
    SECURITY_ATTRIBUTES security = {sizeof(security), NULL, TRUE};
    STARTUPINFOW startup;
    PROCESS_INFORMATION process;
    HANDLE read_pipe = NULL;
    HANDLE write_pipe = NULL;
    wchar_t *line;
    size_t capacity = 0;
    size_t limit;
    DWORD error = ERROR_SUCCESS;
    if (result == NULL || argv == NULL || argv[0] == NULL || argv[0][0] == '\0') {
        if (result != NULL) {
            result_init(result);
            result->platform_error = ERROR_INVALID_PARAMETER;
        }
        return -1;
    }
    result_init(result);
    limit = output_limit ? output_limit : HL_LINT_PROCESS_DEFAULT_LIMIT;
    line = command_line(argv);
    if (line == NULL) {
        result->platform_error = ERROR_NOT_ENOUGH_MEMORY;
        return -1;
    }
    if (!CreatePipe(&read_pipe, &write_pipe, &security, 0) ||
        !SetHandleInformation(read_pipe, HANDLE_FLAG_INHERIT, 0)) {
        error = GetLastError();
        goto done;
    }
    memset(&startup, 0, sizeof(startup));
    memset(&process, 0, sizeof(process));
    startup.cb = sizeof(startup);
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
    startup.hStdOutput = write_pipe;
    startup.hStdError = write_pipe;
    if (!CreateProcessW(NULL, line, NULL, NULL, TRUE, CREATE_NO_WINDOW, NULL, NULL, &startup, &process)) {
        error = GetLastError();
        goto done;
    }
    CloseHandle(write_pipe);
    write_pipe = NULL;
    for (;;) {
        char buffer[8192];
        DWORD count = 0;
        if (ReadFile(read_pipe, buffer, sizeof(buffer), &count, NULL)) {
            if (count == 0) break;
            if (append_output(result, buffer, count, limit, &capacity) != 0) {
                error = ERROR_NOT_ENOUGH_MEMORY;
                break;
            }
            continue;
        }
        error = GetLastError();
        if (error == ERROR_BROKEN_PIPE) error = ERROR_SUCCESS;
        break;
    }
    WaitForSingleObject(process.hProcess, INFINITE);
    {
        DWORD exit_code;
        if (!GetExitCodeProcess(process.hProcess, &exit_code) || exit_code > INT_MAX) {
            if (error == ERROR_SUCCESS) error = GetLastError();
        } else {
            result->exit_code = (int)exit_code;
        }
    }
    CloseHandle(process.hThread);
    CloseHandle(process.hProcess);
done:
    if (write_pipe != NULL) CloseHandle(write_pipe);
    if (read_pipe != NULL) CloseHandle(read_pipe);
    free(line);
    if (error != ERROR_SUCCESS) {
        result->platform_error = (int)error;
        return -1;
    }
    return 0;
}

void hl_lint_process_result_destroy(HlLintProcessResult *result) {
    if (result == NULL) return;
    free(result->output);
    result_init(result);
}
