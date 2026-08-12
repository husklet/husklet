#ifndef HL_LINUX_ABI_GUEST_STAT_H
#define HL_LINUX_ABI_GUEST_STAT_H

#include "encode.h"
#include "host_stat.h" // the struct stat members that are not on every host's structure

#if !defined(HL_GUEST_STAT_SIZE) || !defined(HL_GUEST_STAT_ENCODE) || !defined(HL_GUEST_BOUND_STAT)
#error "guest stat layout macros must be defined by the target"
#endif

static void stat_virt_ids(const struct stat *status, const char *host_path, int descriptor, uint32_t *user,
                          uint32_t *group);
static mode_t stat_virt_mode(const struct stat *status, const char *host_path, int descriptor);

static void fill_linux_stat(uint8_t *destination, const struct stat *status, const char *host_path, int descriptor) {
    uint32_t user, group;
    stat_virt_ids(status, host_path, descriptor, &user, &group);
    hl_linux_stat_record record = {
        status->st_dev,
        status->st_ino,
        status->st_nlink,
        status->st_rdev,
        (uint64_t)status->st_size,
        HL_HOST_STAT_BLOCKS(status),
        HL_HOST_STAT_ATIME_SEC(status),
        HL_HOST_STAT_ATIME_NSEC(status),
        HL_HOST_STAT_MTIME_SEC(status),
        HL_HOST_STAT_MTIME_NSEC(status),
        HL_HOST_STAT_CTIME_SEC(status),
        HL_HOST_STAT_CTIME_NSEC(status),
        stat_virt_mode(status, host_path, descriptor),
        user,
        group,
    };
    (void)HL_GUEST_STAT_ENCODE(&record, destination, HL_GUEST_STAT_SIZE);
}

static void fill_linux_bound_stat(uint8_t *destination, const hl_linux_file_status *status) {
    (void)HL_GUEST_BOUND_STAT(status, destination, HL_GUEST_STAT_SIZE);
}

#endif
