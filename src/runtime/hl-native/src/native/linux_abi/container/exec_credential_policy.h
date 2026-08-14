#ifndef HL_LINUX_EXEC_CREDENTIAL_POLICY_H
#define HL_LINUX_EXEC_CREDENTIAL_POLICY_H

#include <stdint.h>
#include <stddef.h>
#include <errno.h>

typedef struct hl_exec_file_capabilities {
    uint64_t permitted;
    uint64_t inheritable;
    int present;
    int effective;
} hl_exec_file_capabilities;

#define HL_EXEC_CAP_VALID_MASK ((UINT64_C(1) << 41) - UINT64_C(1))

static inline uint32_t hl_exec_capability_word(const unsigned char *bytes) {
    return (uint32_t)bytes[0] | (uint32_t)bytes[1] << 8 | (uint32_t)bytes[2] << 16 | (uint32_t)bytes[3] << 24;
}

static inline int hl_exec_file_capabilities_parse(const unsigned char *bytes, size_t length,
                                                  hl_exec_file_capabilities *capabilities) {
    if (bytes == NULL || capabilities == NULL || (length != 20 && length != 24)) return -EINVAL;
    uint32_t magic = hl_exec_capability_word(bytes);
    uint32_t revision = magic & UINT32_C(0xff000000);
    /* Linux's sansflags() removes only VFS_CAP_FLAGS_EFFECTIVE before
       matching the revision. Every other low flag bit is malformed. */
    if ((magic & ~UINT32_C(1)) != revision ||
        ((revision != UINT32_C(0x02000000) || length != 20) && (revision != UINT32_C(0x03000000) || length != 24)))
        return -EINVAL;
    if (length == 24 && hl_exec_capability_word(bytes + 20) != 0) {
        /* A V3 capability names the user-namespace root that owns it. The
           integrated runtime has one guest root namespace, so another rootid
           is valid metadata but confers nothing here. */
        *capabilities = (hl_exec_file_capabilities){0};
        return 0;
    }
    *capabilities = (hl_exec_file_capabilities){
        (hl_exec_capability_word(bytes + 4) | (uint64_t)hl_exec_capability_word(bytes + 12) << 32) &
            HL_EXEC_CAP_VALID_MASK,
        (hl_exec_capability_word(bytes + 8) | (uint64_t)hl_exec_capability_word(bytes + 16) << 32) &
            HL_EXEC_CAP_VALID_MASK,
        1,
        (magic & 1u) != 0,
    };
    return 0;
}

typedef struct hl_exec_credential_state {
    int ruid, euid, suid;
    int rgid, egid, sgid;
    uint64_t permitted, effective, bounding, inheritable, ambient;
    int securebits;
    int no_new_privileges;
} hl_exec_credential_state;

typedef struct hl_exec_credential_result {
    hl_exec_credential_state state;
    int error;
    int secure_exec;
    int dumpable;
} hl_exec_credential_result;

enum {
    HL_EXEC_SECURE_NOROOT = 1 << 0,
    HL_EXEC_SECURE_NOROOT_LOCKED = 1 << 1,
    HL_EXEC_SECURE_NO_SETUID_FIXUP = 1 << 2,
    HL_EXEC_SECURE_NO_SETUID_FIXUP_LOCKED = 1 << 3,
    HL_EXEC_SECURE_KEEP_CAPS = 1 << 4,
    HL_EXEC_SECURE_KEEP_CAPS_LOCKED = 1 << 5,
    HL_EXEC_SECURE_NO_CAP_AMBIENT_RAISE = 1 << 6,
    HL_EXEC_SECURE_NO_CAP_AMBIENT_RAISE_LOCKED = 1 << 7,
    HL_EXEC_SECURE_ALL = 0xff,
    HL_EXEC_SUID_DUMP_USER = 1,
    HL_EXEC_SUID_DUMP_ROOT = 2
};

static inline hl_exec_credential_result hl_exec_credential_transition(hl_exec_credential_state current, uint32_t mode,
                                                                      uint32_t owner_uid, uint32_t owner_gid,
                                                                      hl_exec_file_capabilities file) {
    hl_exec_credential_result result = {current, 0, 0, HL_EXEC_SUID_DUMP_USER};
    int old_euid = current.euid, old_egid = current.egid;
    if (!current.no_new_privileges) {
        if (mode & 04000u) result.state.euid = (int)owner_uid;
        /* Linux ignores S_ISGID for exec credential purposes unless group
           execute is also set; otherwise the bit is only a locking marker. */
        if ((mode & 02010u) == 02010u) result.state.egid = (int)owner_gid;
    }
    /* Saved IDs are copied after set-id processing on every successful exec. */
    result.state.suid = result.state.euid;
    result.state.sgid = result.state.egid;

    int root_magic =
        !(current.securebits & HL_EXEC_SECURE_NOROOT) && (result.state.ruid == 0 || result.state.euid == 0);
    uint64_t file_permitted = file.present ? file.permitted : 0;
    uint64_t file_inheritable = file.present ? file.inheritable : 0;
    int file_effective = file.present && file.effective;
    uint64_t file_result = (current.inheritable & file_inheritable) | (file_permitted & current.bounding);
    /* An effective file-capability xattr is a forced capability contract.
       Linux refuses exec rather than silently running with a subset. */
    if (file_effective && (file_permitted & ~file_result) != 0) {
        result.error = EPERM;
        return result;
    }
    /* File capabilities win over setuid-root compatibility for a non-root
       caller; Linux deliberately does not combine both privilege sources. */
    int setuid_root_with_file = file.present && result.state.ruid != 0 && result.state.euid == 0;
    if (root_magic && !setuid_root_with_file) {
        file_permitted = current.bounding;
        file_inheritable = UINT64_MAX;
        /* Linux's root compatibility rules are deliberately asymmetric: a
           real or effective UID of zero supplies notional file capability
           sets, but only an effective UID of zero supplies the notional
           effective bit. */
        if (result.state.euid == 0) file_effective = 1;
    }
    uint64_t ambient = current.ambient;
    int ids_changed = result.state.euid != old_euid || result.state.egid != old_egid;
    if (ids_changed || (mode & (04000u | 02000u)) || file.present) ambient = 0;
    uint64_t permitted = (current.inheritable & file_inheritable) | (file_permitted & current.bounding) | ambient;
    uint64_t effective = file_effective ? permitted : ambient;
    if (current.no_new_privileges) {
        permitted &= current.permitted;
        effective &= current.permitted;
    }
    result.state.permitted = permitted;
    result.state.effective = effective;
    result.state.ambient = ambient;
    /* KEEP_CAPS is the one securebit base flag the kernel clears at exec;
       its lock bit and every other securebit survive. */
    result.state.securebits &= ~HL_EXEC_SECURE_KEEP_CAPS;
    /* bprm secure-exec is about the post-exec effective identity differing
       from the task's real identity, not whether exec changed the old euid. */
    int identity_mismatch = result.state.euid != current.ruid || result.state.egid != current.rgid;
    result.secure_exec = !current.no_new_privileges &&
                         (identity_mismatch || (file.present && (permitted != 0 || file.effective)));
    result.dumpable = result.secure_exec ? HL_EXEC_SUID_DUMP_ROOT : HL_EXEC_SUID_DUMP_USER;
    return result;
}

#endif
