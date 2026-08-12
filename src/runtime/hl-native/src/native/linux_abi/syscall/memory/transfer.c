    case 270: {
        // flags (a5) must be 0 -- no process_vm_readv flags are defined, and the kernel rejects any non-zero
        // value with EINVAL BEFORE it touches the iovecs (mm/process_vm_access.c). A silent accept here let a
        // probe with a junk flag read as supported.
        if (a5) {
            G_RET(c) = (uint64_t)(int64_t)-EINVAL;
            break;
        }
        // The iovec arrays themselves are guest buffers the kernel reads via copy_from_user: cap the count at
        // UIO_MAXIOV (EINVAL) and reject an unmapped array (EFAULT) so a bad pointer never faults the engine.
        if ((unsigned long)a2 > 1024 || (unsigned long)a4 > 1024) {
            G_RET(c) = (uint64_t)(int64_t)-EINVAL;
            break;
        }
        struct iovec local_iov[1024], remote_iov[1024];
        if (guest_iov_import(a1, (size_t)a2, local_iov) < 0 || guest_iov_import(a3, (size_t)a4, remote_iov) < 0) {
            G_RET(c) = (uint64_t)(int64_t)-EFAULT;
            break;
        }
        long pr = ptrace_pvm(c, 0, (pid_t)(int)a0, local_iov, (unsigned long)a2, remote_iov, (unsigned long)a4);
        if (pr != PT_PVM_LOCAL) {
            G_RET(c) = (uint64_t)pr;
            break;
        }
        G_RET(c) = (uint64_t)svc_vm_iov_copy(local_iov, (unsigned long)a2, remote_iov, (unsigned long)a4);
        break;
    }
    // process_vm_writev: the mirror -- copy FROM the local iovecs (a1/a2) INTO the remote iovecs (a3/a4).
    case 271: {
        // flags (a5) must be 0, rejected with EINVAL before the iovecs are read (mirrors process_vm_readv).
        if (a5) {
            G_RET(c) = (uint64_t)(int64_t)-EINVAL;
            break;
        }
        if ((unsigned long)a2 > 1024 || (unsigned long)a4 > 1024) {
            G_RET(c) = (uint64_t)(int64_t)-EINVAL;
            break;
        }
        struct iovec local_iov[1024], remote_iov[1024];
        if (guest_iov_import(a1, (size_t)a2, local_iov) < 0 || guest_iov_import(a3, (size_t)a4, remote_iov) < 0) {
            G_RET(c) = (uint64_t)(int64_t)-EFAULT;
            break;
        }
        long pr = ptrace_pvm(c, 1, (pid_t)(int)a0, local_iov, (unsigned long)a2, remote_iov, (unsigned long)a4);
        if (pr != PT_PVM_LOCAL) {
            G_RET(c) = (uint64_t)pr;
            break;
        }
        G_RET(c) = (uint64_t)svc_vm_iov_copy(remote_iov, (unsigned long)a4, local_iov, (unsigned long)a2);
        break;
    }
    // membarrier: CMD_QUERY(0) returns the bitmask of supported commands; the barrier commands issue a
    // process-wide full memory barrier. The host is cache-coherent and a seq-cst fence orders all threads,
    // so every (expedited or not, global or private) barrier is satisfied by a single host fence. The
    // REGISTER_* commands only arm the kernel's per-mm expedited fast path -- there is nothing to register
    // here, so they succeed as a no-op. SYNC_CORE variants additionally guarantee instruction-cache
    // coherence for self-modifying code; the guest's own JIT already flushes via its code-patch path, so a
    // fence suffices. glibc/Go/HAProxy probe QUERY, then REGISTER_PRIVATE_EXPEDITED(16) + PRIVATE_EXPEDITED(8).
