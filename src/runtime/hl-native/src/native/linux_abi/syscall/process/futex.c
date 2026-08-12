// Cohesive process-syscall handlers. Included by ../proc.c after shared process state.
static int svc_proc_98(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 98: { // futex(uaddr, op, val, timeout|nr_wake2=a3, uaddr2=a4, val3=a5)
        const unsigned raw_operation = (unsigned)a1;
        const unsigned known_operation_bits = 0x7fu | 0x80u /* PRIVATE */ | 0x100u /* CLOCK_REALTIME */;
        if (raw_operation & ~known_operation_bits) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        int operation = (int)raw_operation & 0x7f;
        // Linux accepts FUTEX_CLOCK_REALTIME only for the absolute-time
        // operations.  In particular WAIT|CLOCK_REALTIME is ENOSYS; silently
        // dropping the flag turns it into a different wait operation.
        if ((raw_operation & 0x100u) != 0 && operation != 9 && operation != 11 && operation != 13) {
            G_RET(c) = (uint64_t)(int64_t)(-ENOSYS);
            break;
        }
        void *primary = NULL, *secondary = NULL, *timeout = (void *)(uintptr_t)a3;
        hl_logical_vma_pin primary_pin = {0}, secondary_pin = {0}, timeout_pin = {0};
        if (a0 & 3) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        uint32_t primary_access = HL_LOGICAL_VMA_READ;
        if (operation == 6 || operation == 7 || operation == 8 || operation == 13)
            primary_access |= HL_LOGICAL_VMA_WRITE;
        if (guest_atomic_address(a0, sizeof(uint32_t), primary_access, &primary, &primary_pin) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (a4 && (operation == 3 || operation == 4 || operation == 5 || operation == 11 || operation == 12)) {
            if (a4 & 3) {
                hl_logical_vma_unpin(&primary_pin);
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            uint32_t secondary_access = HL_LOGICAL_VMA_READ;
            if (operation == 5 || operation == 11 || operation == 12) secondary_access |= HL_LOGICAL_VMA_WRITE;
            if (guest_atomic_address(a4, sizeof(uint32_t), secondary_access, &secondary, &secondary_pin) < 0) {
                hl_logical_vma_unpin(&primary_pin);
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        } else {
            secondary = (void *)(uintptr_t)a4;
        }
        if (a3 && (operation == 0 || operation == 6 || operation == 9 || operation == 11 || operation == 13) &&
            guest_atomic_address(a3, sizeof(struct timespec), HL_LOGICAL_VMA_READ, &timeout, &timeout_pin) < 0) {
            hl_logical_vma_unpin(&primary_pin);
            hl_logical_vma_unpin(&secondary_pin);
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        // a3 is a timespec* for waits and a wake count for WAKE_OP; operation selects its interpretation.
        int is_private = (raw_operation & 0x80u) != 0;
        /*
         * futex_op's bucket/key helpers canonicalize addresses which belong to
         * MAP_SHARED file mappings. FUTEX_PRIVATE_FLAG must instead remain
         * keyed by this process's virtual address: two aliases of one memfd
         * are distinct private futexes. Move private identity into the
         * non-canonical host half so the shared-object registry cannot fold it
         * back to (dev,inode,offset), while preserving a stable one-to-one key.
         */
        const uintptr_t private_tag = UINT64_C(0x8000000000000000);
        const void *primary_key = is_private ? (const void *)(uintptr_t)(a0 ^ private_tag) : primary;
        const void *secondary_key = is_private ? (const void *)(uintptr_t)(a4 ^ private_tag) : secondary;
        G_RET(c) = (uint64_t)futex_op(c, primary, primary_key, operation, is_private, (int)a2, timeout, (int)a3,
                                      secondary, secondary_key, (uint32_t)a5);
        hl_logical_vma_unpin(&timeout_pin);
        hl_logical_vma_unpin(&secondary_pin);
        hl_logical_vma_unpin(&primary_pin);
        break;
    }
    // set_robust_list(head, len): record the per-thread robust-list head (walked on exit to mark OWNER_DIED +
    // wake robust-mutex waiters). Linux rejects len != sizeof(struct robust_list_head) (24 on LP64).
    default: return 0;
    }
    return 1;
}
static int svc_proc_99(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 99:
        if ((size_t)a1 != 24) {
            G_RET(c) = (uint64_t)(-EINVAL);
        } else {
            c->robust_list = a0;
            G_RET(c) = 0;
        }
        break;
    // syslog
    default: return 0;
    }
    return 1;
}

static int svc_proc_116(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 116: G_RET(c) = 0; break;
    // sched_setaffinity(pid, size, MASK=a2) -- record the requested mask (intersected with the online
    // set) so a later getaffinity reflects the pin; -EINVAL if it selects no online CPU, as on Linux.
    default: return 0;
    }
    return 1;
}
