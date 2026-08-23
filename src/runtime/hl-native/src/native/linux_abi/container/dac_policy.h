#ifndef HL_LINUX_DAC_POLICY_H
#define HL_LINUX_DAC_POLICY_H

#include <errno.h>
#include <stddef.h>
#include <stdint.h>

/* Pure virtual-DAC policy.  Host ids and host permission checks are deliberately
 * absent: callers snapshot the guest-visible inode metadata before asking. */
typedef struct hl_dac_snapshot {
    uint32_t uid;
    uint32_t gid;
    uint32_t mode;
} hl_dac_snapshot;

typedef struct hl_dac_credentials {
    uint32_t fsuid;
    uint32_t fsgid;
    const uint32_t *groups;
    size_t group_count;
    uint64_t capabilities;
} hl_dac_credentials;

enum {
    HL_DAC_CAP_CHOWN = 0,
    HL_DAC_CAP_DAC_OVERRIDE = 1,
    HL_DAC_CAP_DAC_READ_SEARCH = 2,
    HL_DAC_CAP_FOWNER = 3,
};

static inline int hl_dac_has_capability(const hl_dac_credentials *credentials, unsigned capability) {
    return (credentials->capabilities & (UINT64_C(1) << capability)) != 0;
}

static inline int hl_dac_in_group(const hl_dac_credentials *credentials, uint32_t gid) {
    if (credentials->fsgid == gid) return 1;
    for (size_t index = 0; index < credentials->group_count; ++index)
        if (credentials->groups[index] == gid) return 1;
    return 0;
}

static inline int hl_dac_authorize_chmod(const hl_dac_snapshot *inode, const hl_dac_credentials *credentials) {
    return credentials->fsuid == inode->uid || hl_dac_has_capability(credentials, HL_DAC_CAP_FOWNER) ? 0 : EPERM;
}

static inline int hl_dac_authorize_chown(const hl_dac_snapshot *inode, const hl_dac_credentials *credentials,
                                         int64_t requested_uid, int64_t requested_gid) {
    if (hl_dac_has_capability(credentials, HL_DAC_CAP_CHOWN)) return 0;
    if (credentials->fsuid != inode->uid) return EPERM;
    if (requested_uid >= 0 && (uint32_t)requested_uid != inode->uid) return EPERM;
    if (requested_gid >= 0 && !hl_dac_in_group(credentials, (uint32_t)requested_gid)) return EPERM;
    return 0;
}

static inline int hl_dac_authorize_explicit_times(const hl_dac_snapshot *inode, const hl_dac_credentials *credentials) {
    return credentials->fsuid == inode->uid || hl_dac_has_capability(credentials, HL_DAC_CAP_FOWNER) ? 0 : EPERM;
}

static inline int hl_dac_authorize_now_times(const hl_dac_snapshot *inode, const hl_dac_credentials *credentials) {
    if (credentials->fsuid == inode->uid || hl_dac_has_capability(credentials, HL_DAC_CAP_FOWNER) ||
        hl_dac_has_capability(credentials, HL_DAC_CAP_DAC_OVERRIDE))
        return 0;
    unsigned shift = hl_dac_in_group(credentials, inode->gid) ? 3 : 0;
    return ((inode->mode >> shift) & 2u) != 0 ? 0 : EACCES;
}

static inline int hl_dac_authorize_create(const hl_dac_snapshot *parent, const hl_dac_credentials *credentials) {
    if (hl_dac_has_capability(credentials, HL_DAC_CAP_DAC_OVERRIDE)) return 0;
    unsigned shift = credentials->fsuid == parent->uid ? 6 : hl_dac_in_group(credentials, parent->gid) ? 3 : 0;
    unsigned permissions = (parent->mode >> shift) & 7u;
    return (permissions & 3u) == 3u ? 0 : EACCES; /* parent needs W|X */
}

enum { HL_DAC_READ = 4, HL_DAC_WRITE = 2, HL_DAC_EXECUTE = 1 };

static inline int hl_dac_authorize_access(const hl_dac_snapshot *inode, const hl_dac_credentials *credentials,
                                          unsigned requested) {
    if (hl_dac_has_capability(credentials, HL_DAC_CAP_DAC_OVERRIDE)) {
        /* CAP_DAC_OVERRIDE does not manufacture execute permission for a regular file. */
        if ((requested & HL_DAC_EXECUTE) == 0 || (inode->mode & 0170000u) == 0040000u || (inode->mode & 0111u) != 0)
            return 0;
    }
    if (hl_dac_has_capability(credentials, HL_DAC_CAP_DAC_READ_SEARCH) &&
        ((requested & ~HL_DAC_READ) == 0 || ((inode->mode & 0170000u) == 0040000u && requested == HL_DAC_EXECUTE)))
        return 0;
    unsigned shift = credentials->fsuid == inode->uid ? 6 : hl_dac_in_group(credentials, inode->gid) ? 3 : 0;
    unsigned permissions = (inode->mode >> shift) & 7u;
    return (permissions & requested) == requested ? 0 : EACCES;
}

static inline int hl_dac_authorize_sticky(const hl_dac_snapshot *parent, const hl_dac_snapshot *entry,
                                          const hl_dac_credentials *credentials) {
    if ((parent->mode & 01000u) == 0 || credentials->fsuid == parent->uid || credentials->fsuid == entry->uid ||
        hl_dac_has_capability(credentials, HL_DAC_CAP_FOWNER))
        return 0;
    return EPERM;
}

#endif
