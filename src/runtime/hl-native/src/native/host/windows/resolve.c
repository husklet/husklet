/*
 * Confined name resolution on a Windows host: "resolve this relative path
 * beneath this pinned directory, and do not let a symlink escape it".
 *
 * The POSIX implementation walks the path one component at a time with openat()
 * and O_NOFOLLOW, holding a directory descriptor at each step. That structure
 * is not decoration -- it is what makes the confinement sound. Because every
 * step is relative to a descriptor the caller already holds, no window exists in
 * which a component could be swapped for a symlink between the check and the
 * use. A resolution that instead built a string and handed it to the kernel
 * once would be exactly the TOCTOU race this function exists to close.
 *
 * That structure needs two things this host does not have:
 *
 *   - a directory descriptor. The CRT cannot open a directory at all
 *     (open(".", O_RDONLY) fails EACCES, measured), so there is no object to
 *     pin a step against and no openat to make the next step relative to.
 *   - O_NOFOLLOW. There is no open flag that refuses a reparse point; the
 *     nearest equivalent, FILE_FLAG_OPEN_REPARSE_POINT, exists on CreateFileW,
 *     which is not the CRT's open and does not produce a CRT descriptor.
 *
 * The honest Windows implementation is therefore a different one: NtCreateFile
 * with a RootDirectory in the OBJECT_ATTRIBUTES gives a genuine *at() step, and
 * FILE_OPEN_REPARSE_POINT gives the O_NOFOLLOW half -- so the confinement can be
 * rebuilt with the same soundness, over HANDLEs rather than descriptors. What
 * blocks it today is not the walk but the boundary: this signature speaks int
 * descriptors on both sides, and on this host a descriptor in the layer above
 * names no host object. That is the same descriptor-to-HANDLE table gap the
 * memory and directory groups record.
 *
 * So this is a REFUSAL, and a refusal is the only safe answer here. Every other
 * outcome is worse in a specific way: resolving without the confinement would
 * hand back a path that a symlink may have taken outside the root, which is a
 * container escape; and succeeding with target_fd = -1 would look to a caller
 * like a successful resolution of a name that does not exist.
 */

#include "../resolve.h"

#include <errno.h>
#include <stdlib.h>

int hl_host_resolve_beneath(int root_fd, const char *path, unsigned policy, int target_open_flags,
                            hl_host_resolved_path *result) {
    (void)root_fd;
    (void)path;
    (void)policy;
    (void)target_open_flags;
    if (result != NULL) {
        result->parent_fd = -1;
        result->target_fd = -1;
        result->leaf = NULL;
    }
    errno = ENOSYS;
    return -1;
}

/* Destroy stays total even though nothing here ever constructs a result: a
 * caller is entitled to destroy a zeroed structure, and on any host the free
 * must tolerate the leaf being NULL. */
void hl_host_resolved_path_destroy(hl_host_resolved_path *result) {
    if (result == NULL) return;
    free(result->leaf);
    result->leaf = NULL;
    result->parent_fd = -1;
    result->target_fd = -1;
}
