#include "../directory.h"

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/event.h>
#include <unistd.h>

typedef struct hl_macos_directory {
    int queue;

    struct {
        int descriptor;
        uint64_t token;
    } entries[256];
} hl_macos_directory;

int hl_host_directory_init(hl_host_directory *directory) {
    if (directory == NULL) {
        errno = EINVAL;
        return -1;
    }
    hl_macos_directory *state = calloc(1, sizeof(*state));
    if (state == NULL) return -1;
    state->queue = kqueue();
    if (state->queue < 0) {
        free(state);
        return -1;
    }
    (void)fcntl(state->queue, F_SETFD, FD_CLOEXEC);
    directory->state = state;
    return 0;
}

static uint32_t hl_macos_directory_flags(uint32_t interests) {
    uint32_t flags = 0;
    if ((interests & (HL_HOST_DIRECTORY_MODIFY | HL_HOST_DIRECTORY_ATTRIB)) != 0)
        flags |= NOTE_WRITE | NOTE_EXTEND | NOTE_ATTRIB;
    if ((interests & (HL_HOST_DIRECTORY_CREATE | HL_HOST_DIRECTORY_DELETE)) != 0) flags |= NOTE_WRITE | NOTE_LINK;
    if ((interests & HL_HOST_DIRECTORY_RENAME) != 0) flags |= NOTE_RENAME | NOTE_WRITE;
    if ((interests & HL_HOST_DIRECTORY_ACCESS) != 0) flags |= NOTE_ATTRIB;
    if (flags == 0) flags = NOTE_WRITE | NOTE_DELETE | NOTE_RENAME | NOTE_ATTRIB | NOTE_EXTEND | NOTE_LINK;
    return flags;
}

int hl_host_directory_set(hl_host_directory *directory, int descriptor, uint64_t token, uint32_t interests) {
    hl_macos_directory *state = directory == NULL ? NULL : directory->state;
    if (state == NULL || descriptor < 0 || token == 0) {
        errno = EINVAL;
        return -1;
    }
    struct kevent change;
    EV_SET(&change, descriptor, EVFILT_VNODE, EV_ADD | EV_CLEAR, hl_macos_directory_flags(interests), 0,
           (void *)(uintptr_t)token);
    if (kevent(state->queue, &change, 1, NULL, 0, NULL) != 0) return -1;
    for (size_t index = 0; index < 256; ++index)
        if (state->entries[index].token == token || state->entries[index].token == 0) {
            state->entries[index].descriptor = descriptor;
            state->entries[index].token = token;
            return 0;
        }
    struct kevent remove;
    EV_SET(&remove, descriptor, EVFILT_VNODE, EV_DELETE, 0, 0, NULL);
    (void)kevent(state->queue, &remove, 1, NULL, 0, NULL);
    errno = ENOSPC;
    return -1;
}

int hl_host_directory_remove(hl_host_directory *directory, uint64_t token) {
    hl_macos_directory *state = directory == NULL ? NULL : directory->state;
    if (state == NULL || token == 0) {
        errno = EINVAL;
        return -1;
    }
    for (size_t index = 0; index < 256; ++index)
        if (state->entries[index].token == token) {
            struct kevent change;
            EV_SET(&change, state->entries[index].descriptor, EVFILT_VNODE, EV_DELETE, 0, 0, NULL);
            int result = kevent(state->queue, &change, 1, NULL, 0, NULL);
            state->entries[index].token = 0;
            return result;
        }
    errno = ENOENT;
    return -1;
}

int hl_host_directory_wait(hl_host_directory *directory, uint64_t *token) {
    hl_macos_directory *state = directory == NULL ? NULL : directory->state;
    if (state == NULL || token == NULL) {
        errno = EINVAL;
        return -1;
    }
    struct kevent event;
    int count;
    do {
        count = kevent(state->queue, NULL, 0, &event, 1, NULL);
    } while (count < 0 && errno == EINTR);
    if (count > 0) *token = (uint64_t)(uintptr_t)event.udata;
    return count;
}

int hl_host_directory_descriptor(const hl_host_directory *directory) {
    const hl_macos_directory *state = directory == NULL ? NULL : directory->state;
    return state == NULL ? -1 : state->queue;
}

int hl_host_directory_relocate(hl_host_directory *directory, int collision) {
    hl_macos_directory *state = directory == NULL ? NULL : directory->state;
    if (state == NULL || state->queue != collision) return 0;
    int replacement = fcntl(state->queue, F_DUPFD_CLOEXEC, 1 << 20);
    if (replacement < 0) replacement = fcntl(state->queue, F_DUPFD_CLOEXEC, 64);
    if (replacement < 0) return -1;
    close(state->queue);
    state->queue = replacement;
    return 0;
}

void hl_host_directory_close(hl_host_directory *directory) {
    hl_macos_directory *state = directory == NULL ? NULL : directory->state;
    if (state == NULL) return;
    close(state->queue);
    free(state);
    directory->state = NULL;
}

void hl_host_directory_abandon(hl_host_directory *directory) {
    if (directory != NULL) directory->state = NULL;
}
