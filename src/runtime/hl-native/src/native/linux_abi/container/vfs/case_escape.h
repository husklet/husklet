// hl/linux_abi/container/vfs -- the reverse of the case escape, for every path that PRESENTS a
// stored name to the guest.
//
// `case_names.h` owns the forward direction: on a case-folding host it hex-escapes a component
// whose case-insensitive twin already exists, so `Makefile` and `makefile` can be siblings on APFS.
// Anything that hands a stored name back to the guest -- getdents64, a provider-bound directory
// read, an inotify name -- must undo that, or the guest sees `.hl-case-v1-<hex>` where its own file
// name belongs and a glob-then-open reads a name that is not the name.
//
// Decoding is unambiguous because the forward direction is total: `hl_case_name_requires_encoding`
// escapes ANY name that already starts with the prefix, so a guest-chosen `.hl-case-v1-...` is
// itself stored escaped and never presents raw. A physical entry is therefore an engine escape
// exactly when it decodes, and a raw entry is a name the guest chose. No lookup, and no re-resolved
// path string, is involved.
//
// Split out of `case_names.h` so a presentation site can take the reverse direction alone: the
// forward direction owns a filesystem query and an enumerating fallback that a listing must not
// perform per entry.
#ifndef HL_LINUX_VFS_CASE_ESCAPE_H
#define HL_LINUX_VFS_CASE_ESCAPE_H

#include <stddef.h>
#include <string.h>

#ifdef __APPLE__

#define HL_CASE_NAME_PREFIX ".hl-case-v1-"

static inline int hl_case_hex(unsigned char value) {
    if (value >= '0' && value <= '9') return value - '0';
    if (value >= 'a' && value <= 'f') return value - 'a' + 10;
    if (value >= 'A' && value <= 'F') return value - 'A' + 10;
    return -1;
}

static inline int hl_case_name_decode(const char *physical, char *guest, size_t capacity) {
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

#else

static inline int hl_case_name_decode(const char *physical, char *guest, size_t capacity) {
    (void)physical;
    (void)guest;
    (void)capacity;
    return 0;
}

#endif

// The guest-visible spelling of one stored directory entry. `storage` holds the decoded bytes when
// the entry is an escape; the returned pointer aliases `physical` otherwise, so a caller that only
// reads the name copies nothing. `storage` must hold NAME_MAX + 1 bytes.
static inline const char *hl_case_visible(const char *physical, char *storage, size_t capacity) {
    return hl_case_name_decode(physical, storage, capacity) ? storage : physical;
}

#endif /* HL_LINUX_VFS_CASE_ESCAPE_H */
