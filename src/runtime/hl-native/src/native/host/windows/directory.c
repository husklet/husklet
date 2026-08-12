/*
 * Directory change notification on a Windows host.
 *
 * Every entry point here is a REFUSAL, and unusually for this port that is not
 * because the host lacks the mechanism. Windows has two of them --
 * ReadDirectoryChangesW over a directory HANDLE, and the same call in its
 * asynchronous form over a completion port -- and either could carry the
 * ACCESS/MODIFY/CREATE/DELETE/RENAME/ATTRIB interests this group declares.
 *
 * What is missing is the two things underneath:
 *
 *   - the group is keyed on an int descriptor. hl_host_directory_set() takes
 *     one and every caller supplies a number that came out of this layer's own
 *     descriptor namespace, which on this host names no host object. Until the
 *     descriptor-to-HANDLE table exists there is nothing to open the directory
 *     from -- the same gap host_mman.h records for a file-backed mmap.
 *
 *   - hl_host_directory_wait() and hl_host_directory_descriptor() present the
 *     whole group as ONE waitable descriptor that joins the engine's readiness
 *     set. That set is a mixed one on this host and has no single call that
 *     waits on it, which is the refusal host_poll.h documents at length. A
 *     completion port is the natural Windows shape for this and is where the
 *     port lands, but it is a change to how readiness is expressed rather than
 *     an implementation of this signature.
 *
 * So the refusals are honest rather than lazy, and they are all -1: reporting
 * success with no events would make a guest inotify watch look armed and
 * silent, which is indistinguishable from a filesystem where nothing changes.
 */

#include "../directory.h"

#include <errno.h>
#include <stddef.h>

int hl_host_directory_init(hl_host_directory *directory) {
    if (directory != NULL) directory->state = NULL;
    errno = ENOSYS;
    return -1;
}

int hl_host_directory_set(hl_host_directory *directory, int descriptor, uint64_t token, uint32_t interests) {
    (void)directory;
    (void)descriptor;
    (void)token;
    (void)interests;
    errno = ENOSYS;
    return -1;
}

int hl_host_directory_remove(hl_host_directory *directory, uint64_t token) {
    (void)directory;
    (void)token;
    errno = ENOSYS;
    return -1;
}

int hl_host_directory_wait(hl_host_directory *directory, uint64_t *token) {
    (void)directory;
    if (token != NULL) *token = 0;
    errno = ENOSYS;
    return -1;
}

int hl_host_directory_descriptor(const hl_host_directory *directory) {
    (void)directory;
    return -1;
}

int hl_host_directory_relocate(hl_host_directory *directory, int collision) {
    (void)directory;
    (void)collision;
    errno = ENOSYS;
    return -1;
}

void hl_host_directory_close(hl_host_directory *directory) {
    if (directory != NULL) directory->state = NULL;
}

void hl_host_directory_abandon(hl_host_directory *directory) {
    if (directory != NULL) directory->state = NULL;
}
