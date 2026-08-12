#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include "../directory.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/inotify.h>
#include <unistd.h>

#define HL_LINUX_DIRECTORY_CAPACITY 256

typedef struct hl_linux_directory_entry {
    int watch;
    uint64_t token;
} hl_linux_directory_entry;

typedef struct hl_linux_directory {
    int descriptor;
    hl_linux_directory_entry entries[HL_LINUX_DIRECTORY_CAPACITY];
    unsigned char pending[4096];
    size_t pending_offset;
    size_t pending_size;
} hl_linux_directory;

int hl_host_directory_init(hl_host_directory *directory) {
    if (directory == NULL) {
        errno = EINVAL;
        return -1;
    }
    hl_linux_directory *state = calloc(1, sizeof(*state));
    if (state == NULL) return -1;
    state->descriptor = inotify_init1(IN_CLOEXEC);
    if (state->descriptor < 0) {
        free(state);
        return -1;
    }
    directory->state = state;
    return 0;
}

static uint32_t hl_linux_directory_flags(uint32_t interests) {
    uint32_t flags = 0;
    if ((interests & HL_HOST_DIRECTORY_ACCESS) != 0) flags |= IN_ACCESS;
    if ((interests & HL_HOST_DIRECTORY_MODIFY) != 0) flags |= IN_MODIFY;
    if ((interests & HL_HOST_DIRECTORY_CREATE) != 0) flags |= IN_CREATE;
    if ((interests & HL_HOST_DIRECTORY_DELETE) != 0) flags |= IN_DELETE | IN_DELETE_SELF;
    if ((interests & HL_HOST_DIRECTORY_RENAME) != 0) flags |= IN_MOVED_FROM | IN_MOVED_TO | IN_MOVE_SELF;
    if ((interests & HL_HOST_DIRECTORY_ATTRIB) != 0) flags |= IN_ATTRIB;
    if (flags == 0) flags = IN_ALL_EVENTS;
    return flags;
}

int hl_host_directory_set(hl_host_directory *directory, int descriptor, uint64_t token, uint32_t interests) {
    hl_linux_directory *state = directory == NULL ? NULL : directory->state;
    if (state == NULL || descriptor < 0 || token == 0) {
        errno = EINVAL;
        return -1;
    }
    char path[64];
    int length = snprintf(path, sizeof(path), "/proc/self/fd/%d", descriptor);
    if (length < 0 || (size_t)length >= sizeof(path)) {
        errno = EINVAL;
        return -1;
    }
    int watch = inotify_add_watch(state->descriptor, path, hl_linux_directory_flags(interests));
    if (watch < 0) return -1;
    for (size_t index = 0; index < HL_LINUX_DIRECTORY_CAPACITY; ++index)
        if (state->entries[index].watch == watch || state->entries[index].token == 0) {
            state->entries[index] = (hl_linux_directory_entry){watch, token};
            return 0;
        }
    (void)inotify_rm_watch(state->descriptor, watch);
    errno = ENOSPC;
    return -1;
}

int hl_host_directory_remove(hl_host_directory *directory, uint64_t token) {
    hl_linux_directory *state = directory == NULL ? NULL : directory->state;
    if (state == NULL || token == 0) {
        errno = EINVAL;
        return -1;
    }
    for (size_t index = 0; index < HL_LINUX_DIRECTORY_CAPACITY; ++index)
        if (state->entries[index].token == token) {
            int result = inotify_rm_watch(state->descriptor, state->entries[index].watch);
            state->entries[index] = (hl_linux_directory_entry){0, 0};
            return result;
        }
    errno = ENOENT;
    return -1;
}

int hl_host_directory_wait(hl_host_directory *directory, uint64_t *token) {
    hl_linux_directory *state = directory == NULL ? NULL : directory->state;
    if (state == NULL || token == NULL) {
        errno = EINVAL;
        return -1;
    }
    for (;;) {
        if (state->pending_offset >= state->pending_size) {
            ssize_t count;
            do {
                count = read(state->descriptor, state->pending, sizeof(state->pending));
            } while (count < 0 && errno == EINTR);
            if (count <= 0) return (int)count;
            state->pending_offset = 0;
            state->pending_size = (size_t)count;
        }
        const struct inotify_event *event = (const struct inotify_event *)(state->pending + state->pending_offset);
        state->pending_offset += sizeof(*event) + event->len;
        for (size_t index = 0; index < HL_LINUX_DIRECTORY_CAPACITY; ++index)
            if (state->entries[index].token != 0 && state->entries[index].watch == event->wd) {
                *token = state->entries[index].token;
                return 1;
            }
    }
}

int hl_host_directory_descriptor(const hl_host_directory *directory) {
    const hl_linux_directory *state = directory == NULL ? NULL : directory->state;
    return state == NULL ? -1 : state->descriptor;
}

int hl_host_directory_relocate(hl_host_directory *directory, int collision) {
    hl_linux_directory *state = directory == NULL ? NULL : directory->state;
    if (state == NULL || state->descriptor != collision) return 0;
    int replacement = fcntl(state->descriptor, F_DUPFD_CLOEXEC, 1 << 20);
    if (replacement < 0) replacement = fcntl(state->descriptor, F_DUPFD_CLOEXEC, 64);
    if (replacement < 0) return -1;
    close(state->descriptor);
    state->descriptor = replacement;
    return 0;
}

void hl_host_directory_close(hl_host_directory *directory) {
    hl_linux_directory *state = directory == NULL ? NULL : directory->state;
    if (state == NULL) return;
    close(state->descriptor);
    free(state);
    directory->state = NULL;
}

void hl_host_directory_abandon(hl_host_directory *directory) {
    if (directory != NULL) directory->state = NULL;
}
