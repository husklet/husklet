#if defined(__APPLE__)
#define _DARWIN_C_SOURCE
#else
#define _POSIX_C_SOURCE 200809L
#endif

#include "resolve.h"

#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#ifndef O_CLOEXEC
#define O_CLOEXEC 0
#endif

typedef struct fd_stack {
    int *fds;
    size_t count;
    size_t capacity;
} fd_stack;

static void stack_destroy(fd_stack *stack) {
    while (stack->count != 0)
        close(stack->fds[--stack->count]);
    free(stack->fds);
}

static int stack_push(fd_stack *stack, int fd) {
    if (stack->count == stack->capacity) {
        size_t capacity = stack->capacity == 0 ? 8 : stack->capacity * 2;
        int *fds = realloc(stack->fds, capacity * sizeof(*fds));
        if (fds == NULL) return -1;
        stack->fds = fds;
        stack->capacity = capacity;
    }
    stack->fds[stack->count++] = fd;
    return 0;
}

static int duplicate_fd(int fd) {
#ifdef F_DUPFD_CLOEXEC
    return fcntl(fd, F_DUPFD_CLOEXEC, 0);
#else
    int copy = dup(fd);
    if (copy >= 0) (void)fcntl(copy, F_SETFD, FD_CLOEXEC);
    return copy;
#endif
}

static char *join_pending(const char *target, const char *rest) {
    size_t a = strlen(target), b = strlen(rest);
    int slash = a != 0 && b != 0 && target[a - 1] != '/';
    char *joined;
    if (a > (size_t)-1 - b - (size_t)slash - 1) {
        errno = ENOMEM;
        return NULL;
    }
    joined = malloc(a + b + (size_t)slash + 1);
    if (joined == NULL) return NULL;
    memcpy(joined, target, a);
    if (slash) joined[a++] = '/';
    memcpy(joined + a, rest, b + 1);
    return joined;
}

static char *read_link(int dirfd, const char *name) {
    size_t capacity = 128;
    for (;;) {
        char *value = malloc(capacity + 1);
        ssize_t size;
        if (value == NULL) return NULL;
        size = readlinkat(dirfd, name, value, capacity);
        if (size < 0) {
            int saved = errno;
            free(value);
            errno = saved;
            return NULL;
        }
        if ((size_t)size < capacity) {
            value[size] = '\0';
            return value;
        }
        free(value);
        if (capacity > (size_t)-1 / 2) {
            errno = ENAMETOOLONG;
            return NULL;
        }
        capacity *= 2;
    }
}

void hl_host_resolved_path_destroy(hl_host_resolved_path *result) {
    if (result == NULL) return;
    if (result->parent_fd >= 0) close(result->parent_fd);
    if (result->target_fd >= 0) close(result->target_fd);
    free(result->leaf);
    result->parent_fd = -1;
    result->target_fd = -1;
    result->leaf = NULL;
}

int hl_host_resolve_beneath(int root_fd, const char *path, unsigned policy, int target_open_flags,
                            hl_host_resolved_path *result) {
    fd_stack stack = {0};
    char *pending = NULL;
    unsigned links = 0;
    int saved;

    if (root_fd < 0 || path == NULL || path[0] == '\0' || path[0] == '/' || result == NULL ||
        (policy & ~(unsigned)(HL_HOST_RESOLVE_NOFOLLOW_FINAL | HL_HOST_RESOLVE_NO_SYMLINKS)) != 0) {
        errno = EINVAL;
        return -1;
    }
    result->parent_fd = -1;
    result->target_fd = -1;
    result->leaf = NULL;
    pending = strdup(path);
    if (pending == NULL) return -1;
    int root = duplicate_fd(root_fd);
    if (root < 0 || stack_push(&stack, root) < 0) {
        if (root >= 0) close(root);
        free(pending);
        return -1;
    }

    for (;;) {
        char *start = pending;
        char *end;
        char *rest;
        while (*start == '/')
            ++start;
        if (*start == '\0') {
            result->leaf = strdup(".");
            if (result->leaf == NULL) goto fail;
            result->parent_fd = stack.fds[stack.count - 1];
            stack.count--;
            break;
        }
        end = start;
        while (*end != '\0' && *end != '/')
            ++end;
        rest = end;
        while (*rest == '/')
            ++rest;
        *end = '\0';

        if (strcmp(start, ".") == 0) {
            char *next = strdup(rest);
            if (next == NULL) goto fail;
            free(pending);
            pending = next;
            continue;
        }
        if (strcmp(start, "..") == 0) {
            if (stack.count > 1) close(stack.fds[--stack.count]);
            char *next = strdup(rest);
            if (next == NULL) goto fail;
            free(pending);
            pending = next;
            continue;
        }

        struct stat status;
        if (fstatat(stack.fds[stack.count - 1], start, &status, AT_SYMLINK_NOFOLLOW) < 0) {
            if (errno == ENOENT && *rest == '\0' && target_open_flags < 0) {
                result->leaf = strdup(start);
                if (result->leaf == NULL) goto fail;
                result->parent_fd = stack.fds[stack.count - 1];
                stack.count--;
                break;
            }
            goto fail;
        }
        if (S_ISLNK(status.st_mode)) {
            char *target;
            char *next;
            if ((policy & HL_HOST_RESOLVE_NO_SYMLINKS) != 0) {
                errno = ELOOP;
                goto fail;
            }
            if (*rest == '\0' && (policy & HL_HOST_RESOLVE_NOFOLLOW_FINAL) != 0) {
                result->leaf = strdup(start);
                if (result->leaf == NULL) goto fail;
                result->parent_fd = stack.fds[stack.count - 1];
                stack.count--;
                break;
            }
            if (++links > 40) {
                errno = ELOOP;
                goto fail;
            }
            target = read_link(stack.fds[stack.count - 1], start);
            if (target == NULL) goto fail;
            if (target[0] == '\0') {
                free(target);
                errno = ENOENT;
                goto fail;
            }
            next = join_pending(target, rest);
            if (next == NULL) {
                free(target);
                goto fail;
            }
            if (target[0] == '/') {
                while (stack.count > 1)
                    close(stack.fds[--stack.count]);
                /* A symlink's absolute target is absolute within the pinned root. */
                while (*next == '/')
                    memmove(next, next + 1, strlen(next));
            }
            free(target);
            free(pending);
            pending = next;
            continue;
        }

        if (*rest == '\0') {
            result->leaf = strdup(start);
            if (result->leaf == NULL) goto fail;
            result->parent_fd = stack.fds[stack.count - 1];
            stack.count--;
            break;
        } else {
            int child = openat(stack.fds[stack.count - 1], start, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
            char *next;
            if (child < 0) goto fail;
            if (stack_push(&stack, child) < 0) {
                close(child);
                goto fail;
            }
            next = strdup(rest);
            if (next == NULL) goto fail;
            free(pending);
            pending = next;
        }
    }

    if (target_open_flags >= 0) {
        result->target_fd = openat(result->parent_fd, result->leaf, target_open_flags | O_NOFOLLOW | O_CLOEXEC);
        if (result->target_fd < 0) goto fail_result;
    }
    free(pending);
    stack_destroy(&stack);
    return 0;

fail_result:
    saved = errno;
    hl_host_resolved_path_destroy(result);
    errno = saved;
fail:
    saved = errno;
    free(pending);
    stack_destroy(&stack);
    errno = saved;
    return -1;
}
