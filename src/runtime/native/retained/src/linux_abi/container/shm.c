#define _POSIX_C_SOURCE 200809L
#include "shm.h"

#include "../../host/libc_compat.h"

#include <stdio.h>
#include <string.h>

/*
 * /dev/shm is a tmpfs DIRECTORY on Linux, and the guest sees it as one: it lists it, stats it, and opens
 * it as O_DIRECTORY.  Only the Linux host happens to have a real one to fall through to -- macOS has no
 * /dev/shm at all, so the bare path used to ENOENT there.  Back the whole subtree with one real host
 * directory on every host: the container's own overlay upper, or a per-namespace directory under /tmp in
 * direct mode (a former flat "<prefix>-<name>" file scheme left the directory itself unbacked).
 */
static const char *shm_dir(const char *root, const char *namespace_key, char *output, size_t capacity) {
    int length = root != NULL && root[0]
                     ? snprintf(output, capacity, "%s/dev/shm", root)
                     : snprintf(output, capacity, "/tmp/.hl-shm-%s",
                                namespace_key != NULL && namespace_key[0] ? namespace_key : "unscoped");
    if (length < 0 || (size_t)length >= capacity) return NULL;
    // Idempotent; the guest may reach a segment before anything created the directory.
    hl_compat_mkdir(output, 01777);
    return output;
}

const char *hl_shm_path(const char *guest, const char *root, const char *namespace_key, char *output, size_t capacity) {
    if (guest == NULL || output == NULL || capacity == 0 || guest[0] != '/') return NULL;
    int exact = strcmp(guest, "/dev/shm") == 0;
    if (!exact && strncmp(guest, "/dev/shm/", 9)) return NULL;
    if (shm_dir(root, namespace_key, output, capacity) == NULL) return NULL;
    if (exact) return output;

    size_t prefix = strlen(output);
    if (prefix + 1 >= capacity - 1) return NULL;
    output[prefix++] = '/';
    int length = (int)prefix + snprintf(output + prefix, capacity - prefix, "%s", guest + 9);
    if (length > (int)capacity - 1) length = (int)capacity - 1;
    for (int index = (int)prefix; index < length; ++index)
        if (output[index] == '/') output[index] = '_'; // a segment name can never escape the shm directory
    return output;
}
