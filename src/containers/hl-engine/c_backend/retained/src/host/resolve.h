#ifndef HL_HOST_RESOLVE_H
#define HL_HOST_RESOLVE_H

#include "hl/host_services.h"

/*
 * A resolved name is represented by a pinned directory and a single leaf.
 * target_fd is -1 unless target_open_flags passed to hl_host_resolve_beneath
 * is non-negative.  In that case the target is opened without following a
 * final symlink as part of the resolution.
 */
typedef struct hl_host_resolved_path {
    int parent_fd;
    int target_fd;
    char *leaf;
} hl_host_resolved_path;

/* path must be relative.  Symlink expansion is limited to 40 links. */
int hl_host_resolve_beneath(int root_fd, const char *path, unsigned policy, int target_open_flags,
                            hl_host_resolved_path *result);
void hl_host_resolved_path_destroy(hl_host_resolved_path *result);

#endif
