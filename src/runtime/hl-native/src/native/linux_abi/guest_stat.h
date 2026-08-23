#ifndef HL_LINUX_ABI_GUEST_STAT_H
#define HL_LINUX_ABI_GUEST_STAT_H

#include "encode.h"
#include "host_stat.h" // the struct stat members that are not on every host's structure
#include "container/vfs/namespace_transaction.h"

#if !defined(HL_GUEST_STAT_SIZE) || !defined(HL_GUEST_STAT_ENCODE) || !defined(HL_GUEST_BOUND_STAT)
#error "guest stat layout macros must be defined by the target"
#endif

static int stat_virt_snapshot(const struct stat *status, const char *host_path, int descriptor, mode_t *mode,
                              uint32_t *user, uint32_t *group);
static void stat_virt_ids_raw(const struct stat *status, const char *host_path, int descriptor, uint32_t *user,
                              uint32_t *group);
static mode_t stat_virt_mode_raw(const struct stat *status, const char *host_path, int descriptor);

static int fill_linux_stat(uint8_t *destination, const struct stat *status, const char *host_path, int descriptor,
                           int nofollow) {
    uint32_t user, group;
    mode_t mode;
    struct stat refreshed;
    const struct stat *encoded = status;
    if (S_ISSOCK(status->st_mode) && (host_path != NULL || descriptor >= 0)) {
#if defined(_WIN32)
        int result = stat_virt_snapshot(status, host_path, descriptor, &mode, &user, &group);
        if (result != 0) return result;
#else
        int result = -EBUSY;
        for (unsigned attempt = 0; attempt < 64; ++attempt) {
            struct namespace_transaction_read read;
            if (namespace_transaction_read_begin(&read) != 0) return -errno;
            if ((descriptor >= 0 ? fstat(descriptor, &refreshed)
                 : nofollow      ? lstat(host_path, &refreshed)
                                 : stat(host_path, &refreshed)) != 0)
                return -errno;
            stat_virt_ids_raw(&refreshed, host_path, descriptor, &user, &group);
            mode = stat_virt_mode_raw(&refreshed, host_path, descriptor);
            if (namespace_transaction_read_validate(&read) == 0) {
                encoded = &refreshed;
                result = 0;
                break;
            }
            result = -errno;
            if (errno != EAGAIN) break;
        }
        if (result != 0) return result;
#endif
    } else {
        int result = stat_virt_snapshot(status, host_path, descriptor, &mode, &user, &group);
        if (result != 0) return result;
    }
    hl_linux_stat_record record = {
        encoded->st_dev,
        encoded->st_ino,
        encoded->st_nlink,
        encoded->st_rdev,
        (uint64_t)encoded->st_size,
        HL_HOST_STAT_BLOCKS(encoded),
        HL_HOST_STAT_ATIME_SEC(encoded),
        HL_HOST_STAT_ATIME_NSEC(encoded),
        HL_HOST_STAT_MTIME_SEC(encoded),
        HL_HOST_STAT_MTIME_NSEC(encoded),
        HL_HOST_STAT_CTIME_SEC(encoded),
        HL_HOST_STAT_CTIME_NSEC(encoded),
        mode,
        user,
        group,
    };
    (void)HL_GUEST_STAT_ENCODE(&record, destination, HL_GUEST_STAT_SIZE);
    return 0;
}

static void fill_linux_bound_stat(uint8_t *destination, const hl_linux_file_status *status) {
    (void)HL_GUEST_BOUND_STAT(status, destination, HL_GUEST_STAT_SIZE);
}

#endif
