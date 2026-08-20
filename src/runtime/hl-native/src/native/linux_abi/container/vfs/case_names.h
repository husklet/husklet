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

#include <dirent.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <strings.h>
#include <unistd.h>

#ifdef __APPLE__
#define HL_CASE_NAME_PREFIX ".hl-case-v1-"

static int hl_case_hex(unsigned char value) {
    if (value >= '0' && value <= '9') return value - '0';
    if (value >= 'a' && value <= 'f') return value - 'a' + 10;
    if (value >= 'A' && value <= 'F') return value - 'A' + 10;
    return -1;
}

static int hl_case_name_decode(const char *physical, char *guest, size_t capacity) {
    size_t prefix = sizeof(HL_CASE_NAME_PREFIX) - 1;
    if (strncmp(physical, HL_CASE_NAME_PREFIX, prefix) != 0) return 0;
    const char *encoded = physical + prefix;
    size_t size = strlen(encoded);
    if (size == 0 || (size & 1) != 0 || size / 2 >= capacity) return 0;
    for (size_t index = 0; index < size; index += 2) {
        int high = hl_case_hex((unsigned char)encoded[index]);
        int low = hl_case_hex((unsigned char)encoded[index + 1]);
        if (high < 0 || low < 0) return 0;
        guest[index / 2] = (char)((high << 4) | low);
        if (guest[index / 2] == 0 || guest[index / 2] == '/') return 0;
    }
    guest[size / 2] = 0;
    return 1;
}

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

static int hl_case_component(int directory, const char *guest, char *physical, size_t capacity) {
    int duplicate = dup(directory);
    DIR *entries = duplicate < 0 ? NULL : fdopendir(duplicate);
    int collision = 0;
    if (entries != NULL) {
        struct dirent *entry;
        while ((entry = readdir(entries)) != NULL) {
            char decoded[256];
            const char *visible =
                hl_case_name_decode(entry->d_name, decoded, sizeof decoded) ? decoded : entry->d_name;
            if (strcmp(visible, guest) == 0) {
                int written = snprintf(physical, capacity, "%s", entry->d_name);
                closedir(entries);
                return written < 0 || (size_t)written >= capacity ? -ENAMETOOLONG : 0;
            }
            if (strcasecmp(visible, guest) == 0) collision = 1;
        }
        closedir(entries);
    } else if (duplicate >= 0) {
        close(duplicate);
    }
    if (collision || hl_case_name_requires_encoding(guest)) return hl_case_name_encode(guest, physical, capacity);
    return snprintf(physical, capacity, "%s", guest) >= (int)capacity ? -ENAMETOOLONG : 0;
}
#else
static int hl_case_name_decode(const char *physical, char *guest, size_t capacity) {
    (void)physical;
    (void)guest;
    (void)capacity;
    return 0;
}
static int hl_case_component(int directory, const char *guest, char *physical, size_t capacity) {
    (void)directory;
    return snprintf(physical, capacity, "%s", guest) >= (int)capacity ? -ENAMETOOLONG : 0;
}
#endif

#endif /* HL_LINUX_VFS_CASE_NAMES_H */
