// hl/linux_abi/container/vfs -- guest-visible component naming on a case-folding host.
//
// APFS folds case, so a Darwin host cannot store `Makefile` and `makefile` as siblings the way the
// guest's namespace requires. `hl_case_component` selects the exact guest-visible sibling of a
// directory and reversibly hex-escapes a component whose case-insensitive twin already exists.
// Every non-Darwin host stores the guest name verbatim, so the same entry points compile to a
// passthrough there.
//
// This lives in a header rather than inside `mounts.c` because two unity fragments call it --
// `mounts.c`/`resolve.c` for whole-path resolution and `cursor.c` for a single component under an
// authority descriptor -- and `cursor.c` is also compiled standalone by
// `tests/cursor_authority.rs`. Relying on `mounts.c` having been #included first made that fixture
// depend on an implicit declaration, which GCC accepted and Apple clang rejects.
#ifndef HL_LINUX_VFS_CASE_NAMES_H
#define HL_LINUX_VFS_CASE_NAMES_H

#include "case_escape.h" // hl_case_name_decode: the reverse direction, shared with every presentation site

#include <dirent.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <strings.h>
#include <unistd.h>

#ifdef __APPLE__
#include <stdint.h>
#include <sys/attr.h>

static int hl_case_name_encode(const char *guest, char *physical, size_t capacity) {
    static const char hex[] = "0123456789abcdef";
    size_t prefix = sizeof(HL_CASE_NAME_PREFIX) - 1;
    size_t size = strlen(guest);
    if (prefix + size * 2 >= capacity) return -ENAMETOOLONG;
    memcpy(physical, HL_CASE_NAME_PREFIX, prefix);
    for (size_t index = 0; index < size; ++index) {
        unsigned char byte = (unsigned char)guest[index];
        physical[prefix + index * 2] = hex[byte >> 4];
        physical[prefix + index * 2 + 1] = hex[byte & 15];
    }
    physical[prefix + size * 2] = 0;
    return 0;
}

static int hl_case_name_requires_encoding(const char *name) {
    if (strncmp(name, HL_CASE_NAME_PREFIX, sizeof(HL_CASE_NAME_PREFIX) - 1) == 0) return 1;
    for (; *name; ++name)
        if (*name >= 'A' && *name <= 'Z') return 1;
    return 0;
}

static int hl_case_store(char *physical, size_t capacity, const char *name) {
    int written = snprintf(physical, capacity, "%s", name);
    return written < 0 || (size_t)written >= capacity ? -ENAMETOOLONG : 0;
}

// The exactly-cased on-disk spelling of `name`, asked of the filesystem in ONE syscall instead of
// enumerating the parent. ATTR_CMN_NAME reports the entry's true name, so a case-folding volume that
// resolves `Foo` onto a stored `foo` still reports `foo` and the fold is detectable. The lookup runs
// against the descriptor the caller already holds (`dirfd`), never against a re-resolved path
// string, so the name it produces describes the same directory the caller then opens into.
// `dirpath` serves only the pre-realpath string walk in `hl_case_path`, which holds no descriptor.
// FSOPT_NOFOLLOW keeps a symlink -- dangling included -- reported as itself, matching readdir.
// Returns 1 with `out` set, 0 when no entry matches under the volume's own folding rules, and -1
// when the volume cannot answer, which sends the caller to the enumerating fallback below.
static int hl_case_true_name(int dirfd, const char *dirpath, const char *name, char *out, size_t capacity) {
    struct attrlist request;
    memset(&request, 0, sizeof request);
    request.bitmapcount = ATTR_BIT_MAP_COUNT;
    request.commonattr = ATTR_CMN_NAME;
    struct {
        uint32_t length;
        attrreference_t name;
        char storage[1024];
    } reply;
    memset(&reply, 0, sizeof reply);
    int rc;
    if (dirfd >= 0) {
        rc = getattrlistat(dirfd, name, &request, &reply, sizeof reply, FSOPT_NOFOLLOW);
    } else {
        char joined[8400];
        if (snprintf(joined, sizeof joined, "%s/%s", dirpath, name) >= (int)sizeof joined) return 0;
        rc = getattrlist(joined, &request, &reply, sizeof reply, FSOPT_NOFOLLOW);
    }
    if (rc != 0) {
        int error = errno;
        return error == ENOENT || error == ENOTDIR || error == ENAMETOOLONG ? 0 : -1;
    }
    if (reply.name.attr_dataoffset < 0 || reply.name.attr_length == 0) return -1;
    const char *found = (const char *)&reply.name + reply.name.attr_dataoffset;
    if (found < reply.storage || found + reply.name.attr_length > reply.storage + sizeof reply.storage) return -1;
    if (found[reply.name.attr_length - 1] != 0) return -1;
    return hl_case_store(out, capacity, found) == 0 ? 1 : -1;
}

// The original enumerating algorithm, retained for a volume whose attribute lookup fails for any
// reason other than "no such entry".
static int hl_case_scan(int dirfd, const char *dirpath, const char *guest, char *physical, size_t capacity) {
    int duplicate = dirfd >= 0 ? dup(dirfd) : -1;
    DIR *entries = dirfd >= 0 ? (duplicate < 0 ? NULL : fdopendir(duplicate)) : opendir(dirpath);
    int collision = 0;
    if (entries != NULL) {
        struct dirent *entry;
        while ((entry = readdir(entries)) != NULL) {
            char decoded[256];
            const char *visible =
                hl_case_name_decode(entry->d_name, decoded, sizeof decoded) ? decoded : entry->d_name;
            if (strcmp(visible, guest) == 0) {
                int stored = hl_case_store(physical, capacity, entry->d_name);
                closedir(entries);
                return stored;
            }
            if (strcasecmp(visible, guest) == 0) collision = 1;
        }
        closedir(entries);
    } else if (duplicate >= 0) {
        close(duplicate);
    }
    if (collision || hl_case_name_requires_encoding(guest)) return hl_case_name_encode(guest, physical, capacity);
    return hl_case_store(physical, capacity, guest);
}

// Only TWO physical spellings can ever present the guest name `guest`: `guest` itself, when it is
// not an escape form, and hl_case_name_encode(guest), because the escape is a deterministic
// lowercase-hex function of the guest bytes. The enumeration above is therefore answerable by at
// most two O(1) lookups whatever the size of the directory. The raw spelling is preferred over the
// escape, which is the choice the enumeration made whenever both existed. A fold onto a
// differently-cased sibling is detected exactly when the volume resolves `guest` to a true name that
// is not `guest`; on a case-sensitive volume that never happens and the guest name is used verbatim,
// which is what Linux would do.
static int hl_case_name(int dirfd, const char *dirpath, const char *guest, char *physical, size_t capacity) {
    char decoded[256], found[1024];
    int collision = 0;
    if (!hl_case_name_decode(guest, decoded, sizeof decoded)) {
        int rc = hl_case_true_name(dirfd, dirpath, guest, found, sizeof found);
        if (rc < 0) return hl_case_scan(dirfd, dirpath, guest, physical, capacity);
        if (rc > 0) {
            if (strcmp(found, guest) == 0) return hl_case_store(physical, capacity, guest);
            collision = 1;
        }
    }
    char encoded[768];
    if (hl_case_name_encode(guest, encoded, sizeof encoded) == 0) {
        int rc = hl_case_true_name(dirfd, dirpath, encoded, found, sizeof found);
        if (rc < 0) return hl_case_scan(dirfd, dirpath, guest, physical, capacity);
        if (rc > 0 && hl_case_name_decode(found, decoded, sizeof decoded) && strcmp(decoded, guest) == 0)
            return hl_case_store(physical, capacity, found);
    }
    if (collision || hl_case_name_requires_encoding(guest)) return hl_case_name_encode(guest, physical, capacity);
    return hl_case_store(physical, capacity, guest);
}

static int hl_case_component(int directory, const char *guest, char *physical, size_t capacity) {
    return hl_case_name(directory, NULL, guest, physical, capacity);
}
#else
static int hl_case_component(int directory, const char *guest, char *physical, size_t capacity) {
    (void)directory;
    return snprintf(physical, capacity, "%s", guest) >= (int)capacity ? -ENAMETOOLONG : 0;
}
#endif

#endif /* HL_LINUX_VFS_CASE_NAMES_H */
