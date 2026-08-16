#ifndef HL_LINUX_ABI_SYSCALL_FS_PROCFD_H
#define HL_LINUX_ABI_SYSCALL_FS_PROCFD_H

#include <stddef.h>
#include <string.h>

static inline int hl_proc_fd_decimal(const char *text, size_t length) {
    if (length == 0) return 0;
    for (size_t index = 0; index < length; ++index)
        if (text[index] < '0' || text[index] > '9') return 0;
    return 1;
}

static inline int hl_proc_fd_anon_name(const char *text, size_t length) {
    if (length == 0) return 0;
    for (size_t index = 0; index < length; ++index) {
        unsigned char byte = (unsigned char)text[index];
        if (!((byte >= 'a' && byte <= 'z') || (byte >= 'A' && byte <= 'Z') || (byte >= '0' && byte <= '9') ||
              byte == '_' || byte == '-'))
            return 0;
    }
    return 1;
}

/* Linux procfs returns non-filesystem spellings for open descriptions that
 * have no pathname. Keep the grammar exact: a Windows drive or UNC path is
 * also not slash-rooted, but remains a filesystem path that must pass through
 * host-to-guest projection and confinement. */
static inline int hl_proc_fd_pseudo_target(const char *target) {
    static const char *const namespaces[] = {
        "cgroup", "ipc", "mnt", "net", "pid", "pid_for_children", "time", "time_for_children", "user", "uts"};
    if (target == NULL) return 0;
    size_t length = strlen(target);
    static const char anon[] = "anon_inode:";
    size_t anon_length = sizeof anon - 1u;
    if (length > anon_length && memcmp(target, anon, anon_length) == 0) {
        const char *name = target + anon_length;
        size_t name_length = length - anon_length;
        if (name_length >= 2u && name[0] == '[' && name[name_length - 1u] == ']') {
            ++name;
            name_length -= 2u;
        }
        return hl_proc_fd_anon_name(name, name_length);
    }
    if (length < 4u || target[length - 1u] != ']') return 0;
    const char *bracket = strchr(target, '[');
    if (bracket == NULL || bracket == target || bracket[-1] != ':' || strchr(bracket + 1, '[') != NULL) return 0;
    size_t kind_length = (size_t)(bracket - target - 1);
    int known = (kind_length == 4u && memcmp(target, "pipe", 4u) == 0) ||
                (kind_length == 6u && memcmp(target, "socket", 6u) == 0);
    for (size_t index = 0; !known && index < sizeof namespaces / sizeof namespaces[0]; ++index)
        known = strlen(namespaces[index]) == kind_length && memcmp(target, namespaces[index], kind_length) == 0;
    if (!known) return 0;
    return hl_proc_fd_decimal(bracket + 1, (size_t)(target + length - 1u - (bracket + 1)));
}

#endif
