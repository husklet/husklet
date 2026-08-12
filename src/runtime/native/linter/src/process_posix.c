#define _POSIX_C_SOURCE 200809L

#include "process.h"

#include <errno.h>
#include <fcntl.h>
#include <spawn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

enum { HL_LINT_PROCESS_DEFAULT_LIMIT = 8 * 1024 * 1024 };

static void result_init(HlLintProcessResult *result) {
    memset(result, 0, sizeof *result);
    result->exit_code = -1;
}

static int set_close_on_exec(int fd) {
    int flags = fcntl(fd, F_GETFD);
    if (flags < 0 || fcntl(fd, F_SETFD, flags | FD_CLOEXEC) < 0) { return -1; }
    return 0;
}

static int append_output(HlLintProcessResult *result, const char *data, size_t length, size_t limit, size_t *capacity) {
    size_t available = result->output_size < limit ? limit - result->output_size : 0;
    size_t keep = length < available ? length : available;
    if (keep < length) { result->output_truncated = true; }
    if (keep == 0) { return 0; }

    size_t required = result->output_size + keep + 1;
    if (required > *capacity) {
        size_t next = *capacity ? *capacity : 4096;
        while (next < required) {
            if (next > (SIZE_MAX / 2)) {
                errno = ENOMEM;
                return -1;
            }
            next *= 2;
        }
        if (next > limit + 1) { next = limit + 1; }
        char *grown = realloc(result->output, next);
        if (!grown) { return -1; }
        result->output = grown;
        *capacity = next;
    }

    memcpy(result->output + result->output_size, data, keep);
    result->output_size += keep;
    result->output[result->output_size] = '\0';
    return 0;
}

int hl_lint_process_run(const char *const argv[], size_t output_limit, HlLintProcessResult *result) {
    if (!result || !argv || !argv[0] || argv[0][0] == '\0') {
        if (result) {
            result_init(result);
            result->platform_error = EINVAL;
        }
        return -1;
    }
    result_init(result);
    size_t limit = output_limit ? output_limit : HL_LINT_PROCESS_DEFAULT_LIMIT;

    int output_pipe[2];
    if (pipe(output_pipe) != 0) {
        result->platform_error = errno;
        return -1;
    }
    if (set_close_on_exec(output_pipe[0]) != 0 || set_close_on_exec(output_pipe[1]) != 0) {
        result->platform_error = errno;
        close(output_pipe[0]);
        close(output_pipe[1]);
        return -1;
    }

    posix_spawn_file_actions_t actions;
    int error = posix_spawn_file_actions_init(&actions);
    if (error == 0) { error = posix_spawn_file_actions_adddup2(&actions, output_pipe[1], STDOUT_FILENO); }
    if (error == 0) { error = posix_spawn_file_actions_adddup2(&actions, output_pipe[1], STDERR_FILENO); }
    if (error == 0) { error = posix_spawn_file_actions_addclose(&actions, output_pipe[0]); }
    if (error == 0) { error = posix_spawn_file_actions_addclose(&actions, output_pipe[1]); }
    if (error != 0) {
        result->platform_error = error;
        posix_spawn_file_actions_destroy(&actions);
        close(output_pipe[0]);
        close(output_pipe[1]);
        return -1;
    }

    pid_t child = -1;
    error = posix_spawnp(&child, argv[0], &actions, NULL, (char *const *)(uintptr_t)argv, environ);
    posix_spawn_file_actions_destroy(&actions);
    close(output_pipe[1]);
    if (error != 0) {
        result->platform_error = error;
        close(output_pipe[0]);
        return -1;
    }

    char buffer[8192];
    size_t capacity = 0;
    int read_error = 0;
    for (;;) {
        ssize_t count = read(output_pipe[0], buffer, sizeof buffer);
        if (count > 0) {
            if (append_output(result, buffer, (size_t)count, limit, &capacity) != 0 && read_error == 0) {
                read_error = errno;
            }
            continue;
        }
        if (count == 0) { break; }
        if (errno == EINTR) { continue; }
        read_error = errno;
        break;
    }
    close(output_pipe[0]);

    int status = 0;
    while (waitpid(child, &status, 0) < 0) {
        if (errno == EINTR) { continue; }
        result->platform_error = errno;
        return -1;
    }
    if (read_error != 0) {
        result->platform_error = read_error;
        return -1;
    }

    if (WIFEXITED(status)) {
        result->exit_code = WEXITSTATUS(status);
    } else if (WIFSIGNALED(status)) {
        result->term_signal = WTERMSIG(status);
    } else {
        result->platform_error = ECHILD;
        return -1;
    }
    return 0;
}

void hl_lint_process_result_destroy(HlLintProcessResult *result) {
    if (!result) { return; }
    free(result->output);
    result_init(result);
}
