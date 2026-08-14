#ifndef HL_LINUX_ABI_SYSCALL_BINDING_VECTOR_VALIDATION_H
#define HL_LINUX_ABI_SYSCALL_BINDING_VECTOR_VALIDATION_H

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

/* Linux validates the aggregate byte count before payload address ranges. */
static inline int hl_guest_iov_validate(uint64_t base, uint64_t size, uint64_t *total) {
    if (total == NULL || size > (uint64_t)INT64_MAX - *total) return -EINVAL;
    *total += size;
    if (size && (base > UINT64_MAX - size || base + size > UINT64_C(0x0001000000000000))) return -EFAULT;
    return 0;
}

#endif
