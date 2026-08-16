#ifndef HL_LINUX_ABI_SYSCALL_BINDING_VECTOR_VALIDATION_H
#define HL_LINUX_ABI_SYSCALL_BINDING_VECTOR_VALIDATION_H

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

/* Linux validates each imported segment's accumulated byte count and address range in sequence. */
static inline int hl_guest_iov_validate(uint64_t base, uint64_t size, uint64_t *total) {
    const uint64_t user_ceiling = UINT64_C(0x0001000000000000);
    if (total == NULL || size > (uint64_t)INT64_MAX - *total) return -EINVAL;
    *total += size;
    /* Linux import_iovec applies access_ok even to an empty segment.  A zero
       length suppresses the copy, not validation of an out-of-user-range base. */
    if (base >= user_ceiling || size > user_ceiling - base) return -EFAULT;
    return 0;
}

#endif
