static int bound_native_pipe(int fd) {
    struct stat metadata;
    return fstat(fd, &metadata) == 0 && S_ISFIFO(metadata.st_mode);
}

static int64_t bound_splice(const hl_linux_fd_snapshot *input, int input_fd, uint64_t input_offset_address,
                            const hl_linux_fd_snapshot *output, int output_fd, uint64_t output_offset_address,
                            uint64_t size, uint64_t flags) {
    off_t input_value = 0, output_value = 0;
    off_t *input_offset = input_offset_address != 0 ? &input_value : NULL;
    off_t *output_offset = output_offset_address != 0 ? &output_value : NULL;
    int input_pipe = input == NULL && bound_native_pipe(input_fd);
    int output_pipe = output == NULL && bound_native_pipe(output_fd);
    static _Thread_local char buffer[65536];
    int64_t read_count, write_count, write_error = 0;
    size_t pushed = 0;
    if (flags & ~UINT64_C(0xf)) return -EINVAL;
    if (!input_pipe && !output_pipe) return -EINVAL;
    if ((input_pipe && input_offset != NULL) || (output_pipe && output_offset != NULL)) return -ESPIPE;
    if ((input_offset != NULL && guest_copy_from(input_offset, input_offset_address, sizeof(*input_offset)) !=
                                     (ssize_t)sizeof(*input_offset)) ||
        (output_offset != NULL && guest_copy_from(output_offset, output_offset_address, sizeof(*output_offset)) !=
                                      (ssize_t)sizeof(*output_offset)))
        return -EFAULT;
    if (size > UINT64_C(0x7ffff000)) size = UINT64_C(0x7ffff000);
    if (size > sizeof(buffer)) size = sizeof(buffer);
    if (size == 0) return 0;
    if (input_pipe) pushed = pipe_pushback_take(input_fd, buffer, (size_t)size);
    read_count = pushed != 0 ? (int64_t)pushed : bound_stream_read(input, input_fd, buffer, (size_t)size, input_offset);
    if (read_count <= 0) return read_count;
    write_count = bound_stream_write(output, output_fd, buffer, (size_t)read_count, output_offset);
    if (write_count < 0) {
        write_error = write_count;
        write_count = 0;
    }
    if (write_count < read_count) {
        size_t remainder = (size_t)(read_count - write_count);
        if (input_pipe)
            pipe_pushback_set(input_fd, buffer + write_count, remainder);
        else if (input_offset == NULL)
            (void)(input != NULL ? hl_linux_lseek(g_linux_box, input->fd, write_count - read_count, SEEK_CUR)
                                 : lseek(input_fd, (off_t)(write_count - read_count), SEEK_CUR));
    }
    if (write_count == 0) return write_error;
    if (input_offset != NULL) *input_offset += (off_t)write_count;
    if (output != NULL)
        bound_mapping_file_written(output, output_offset != NULL ? (uint64_t)*output_offset : output->offset,
                                   (uint64_t)write_count);
    if (output_offset != NULL) *output_offset += (off_t)write_count;
    if ((input_offset != NULL &&
         guest_copy_to(input_offset_address, input_offset, sizeof(*input_offset)) != (ssize_t)sizeof(*input_offset)) ||
        (output_offset != NULL && guest_copy_to(output_offset_address, output_offset, sizeof(*output_offset)) !=
                                      (ssize_t)sizeof(*output_offset)))
        return -EFAULT;
    return write_count;
}

// Enforce the guest soft RLIMIT_FSIZE on a bound-descriptor write of `count` bytes starting at absolute
// offset `pos`. Mirrors the native fsize_gate (io.c): a regular-file write at/beyond the limit raises SIGXFSZ
// and returns -EFBIG; a straddling write is clamped to the limit. Zero cost when the limit is infinite.
static int64_t bound_fsize_gate(struct cpu *c, const hl_linux_fd_snapshot *source, uint64_t pos, uint64_t count) {
    uint64_t limit = guest_fsize_cur();
    if (limit == ~UINT64_C(0) || count == 0) return (int64_t)count;
    if (g_host_services == NULL || g_host_services->file == NULL || g_host_services->file->metadata == NULL)
        return (int64_t)count;
    hl_host_file_metadata metadata;
    hl_host_result status = g_host_services->file->metadata(g_host_services->context, source->host_handle, &metadata);
    if (status.status != HL_STATUS_OK || metadata.type != HL_HOST_FILE_TYPE_REGULAR) return (int64_t)count;
    if (pos >= limit) {
        raise_guest_signal(c, 25); // SIGXFSZ
        return -EFBIG;
    }
    uint64_t room = limit - pos;
    return count > room ? (int64_t)room : (int64_t)count;
}

/* renameat2(RENAME_EXCHANGE) across bound directories.  The host exposes only a
   replacing rename (renameat), so an atomic swap is staged through a private
   temporary in the destination directory: new->temp, old->new, temp->old.  Both
   operands must exist, matching the Linux contract; a failed middle step rolls
   the temporary back so neither name is lost. */
static int64_t bound_rename_exchange(hl_host_handle old_dir, const char *old_path, size_t old_size,
                                     hl_host_handle new_dir, const char *new_path, size_t new_size) {
    static uint64_t counter;
    const hl_host_file_services *file = g_host_services->file;
    void *ctx = g_host_services->context;
    char temp[64];
    int written = snprintf(temp, sizeof temp, ".hl-xchg-%d-%llu", (int)getpid(),
                           (unsigned long long)__atomic_add_fetch(&counter, 1, __ATOMIC_RELAXED));
    if (written <= 0 || (size_t)written >= sizeof temp) return -EIO;
    size_t temp_size = (size_t)written;
    hl_host_result step = file->rename_relative(ctx, new_dir, new_path, new_size, new_dir, temp, temp_size);
    if (step.status != HL_STATUS_OK) return bound_host_error(step.status);
    step = file->rename_relative(ctx, old_dir, old_path, old_size, new_dir, new_path, new_size);
    if (step.status != HL_STATUS_OK) {
        (void)file->rename_relative(ctx, new_dir, temp, temp_size, new_dir, new_path, new_size);
        return bound_host_error(step.status);
    }
    step = file->rename_relative(ctx, new_dir, temp, temp_size, old_dir, old_path, old_size);
    if (step.status != HL_STATUS_OK) return bound_host_error(step.status);
    return 0;
}

#if defined(_WIN32)
/*
 * The descriptor-shaped operations on a socket, on the one host where a socket
 * descriptor is not a kernel descriptor.
 *
 * Everything the socket FAMILY names -- socket, bind, connect, sendmsg and the
 * rest of 198..212 -- already reaches the network group through svc_net and the
 * REAL vocabulary in host_socket.h, and none of it comes through here. What
 * comes through here is the operations that are NOT about sockets and happen to
 * be applied to one: read, write, close, dup, fcntl, ioctl and the two waits.
 * Those resolve to the C library on this host, and the C library's descriptor
 * table does not know that this number names a socket.
 *
 * It is routed here rather than inside those calls because this is the first
 * router the dispatcher consults, and because it runs BEFORE the bound-slot
 * gate below -- a socket descriptor is not a bound slot, and on this host there
 * are no bound slots at all.
 */
static int64_t bound_socket_transfer(uint64_t address, uint64_t count, int descriptor, int writing) {
    void *buffer;
    int64_t result;
    size_t size = count > (uint64_t)(1u << 20) ? (size_t)(1u << 20) : (size_t)count;
    buffer = malloc(size != 0 ? size : 1);
    if (buffer == NULL) return -ENOMEM;
    if (writing) {
        if (size != 0 && guest_copy_from(buffer, address, size) != (ssize_t)size) {
            free(buffer);
            return -EFAULT;
        }
        result = (int64_t)hl_linux_socket_write(descriptor, buffer, size);
    } else {
        result = (int64_t)hl_linux_socket_read(descriptor, buffer, size);
        if (result > 0 && guest_copy_to(address, buffer, (size_t)result) != (ssize_t)result) {
            free(buffer);
            return -EFAULT;
        }
    }
    if (result < 0) result = -(int64_t)errno;
    free(buffer);
    return result;
}

/* readv/writev are coalesced through one buffer rather than issued per vector.
 * That is not an optimisation: a datagram socket must produce or consume one
 * message per call, and a loop over the vectors would turn one datagram into
 * several. */
static int64_t bound_socket_vector(struct cpu *c, uint64_t address, uint64_t count, int descriptor, int writing) {
    struct iovec vectors[64];
    unsigned char *buffer;
    uint64_t total = 0;
    uint64_t index;
    int64_t result;
    (void)c;
    if (count > HL_ARRAY_COUNT(vectors)) return -EINVAL;
    if (count != 0 && guest_copy_from(vectors, address, (size_t)count * sizeof(vectors[0])) !=
                          (ssize_t)((size_t)count * sizeof(vectors[0])))
        return -EFAULT;
    for (index = 0; index < count; ++index) {
        if (vectors[index].iov_len > (size_t)(1u << 20)) return -EINVAL;
        total += (uint64_t)vectors[index].iov_len;
    }
    if (total > (uint64_t)(1u << 22)) return -EINVAL;
    buffer = malloc(total != 0 ? (size_t)total : 1);
    if (buffer == NULL) return -ENOMEM;
    if (writing) {
        uint64_t offset = 0;
        for (index = 0; index < count; ++index) {
            const size_t length = vectors[index].iov_len;
            if (length != 0 && guest_copy_from(buffer + offset, (uint64_t)(uintptr_t)vectors[index].iov_base, length) !=
                                   (ssize_t)length) {
                free(buffer);
                return -EFAULT;
            }
            offset += length;
        }
        result = (int64_t)hl_linux_socket_write(descriptor, buffer, (size_t)total);
    } else {
        result = (int64_t)hl_linux_socket_read(descriptor, buffer, (size_t)total);
        if (result > 0) {
            uint64_t remaining = (uint64_t)result;
            uint64_t offset = 0;
            for (index = 0; index < count && remaining != 0; ++index) {
                size_t length = vectors[index].iov_len;
                if ((uint64_t)length > remaining) length = (size_t)remaining;
                if (length != 0 && guest_copy_to((uint64_t)(uintptr_t)vectors[index].iov_base, buffer + offset,
                                                 length) != (ssize_t)length) {
                    free(buffer);
                    return -EFAULT;
                }
                offset += length;
                remaining -= length;
            }
        }
    }
    if (result < 0) result = -(int64_t)errno;
    free(buffer);
    return result;
}

static short bound_socket_poll_events(uint32_t ready, short requested) {
    short revents = 0;
    if ((ready & HL_HOST_READY_READ) != 0) revents |= (short)(POLLIN & requested);
    if ((ready & HL_HOST_READY_WRITE) != 0) revents |= (short)(POLLOUT & requested);
    /* POLLERR and POLLHUP are reported whether or not they were asked for; that
     * is poll(2)'s rule and the reason a caller may pass events == 0. */
    if ((ready & HL_HOST_READY_ERROR) != 0) revents |= POLLERR;
    if ((ready & HL_HOST_READY_HANGUP) != 0) revents |= (short)(POLLHUP | (POLLRDHUP & requested));
    return revents;
}

/*
 * poll over a set whose every member is a socket. A mixed set is declined and
 * falls through to the ambient path, because the two populations have no shared
 * waitable form on this host yet and half-answering a set is worse than not
 * claiming it.
 *
 * The wait is a bounded re-derivation loop rather than a block, which is the
 * readiness model this contract is built on: nothing here is woken by name, and
 * a caller asks again. The slice is short enough that a guest select() with a
 * millisecond timeout still behaves like one.
 */
static int bound_socket_poll(struct cpu *c, uint64_t address, uint64_t count, int64_t timeout_ms, int64_t *out) {
    struct pollfd entries[64];
    uint64_t index;
    uint64_t elapsed = 0;
    (void)c;
    if (count == 0 || count > HL_ARRAY_COUNT(entries)) return 0;
    if (guest_copy_from(entries, address, (size_t)count * sizeof(entries[0])) !=
        (ssize_t)((size_t)count * sizeof(entries[0])))
        return 0;
    for (index = 0; index < count; ++index)
        if (entries[index].fd >= 0 && !hl_linux_socket_is(entries[index].fd)) return 0;
    for (;;) {
        int ready_count = 0;
        for (index = 0; index < count; ++index) {
            uint32_t ready = 0;
            entries[index].revents = 0;
            if (entries[index].fd < 0) continue;
            if (hl_linux_socket_readiness(entries[index].fd, 0, &ready) != 0) {
                entries[index].revents = POLLNVAL;
                ready_count++;
                continue;
            }
            entries[index].revents = bound_socket_poll_events(ready, entries[index].events);
            if (entries[index].revents != 0) ready_count++;
        }
        if (ready_count != 0 || timeout_ms == 0) {
            if (guest_copy_to(address, entries, (size_t)count * sizeof(entries[0])) !=
                (ssize_t)((size_t)count * sizeof(entries[0])))
                *out = -EFAULT;
            else
                *out = ready_count;
            return 1;
        }
        if (timeout_ms > 0 && (int64_t)elapsed >= timeout_ms) {
            *out = 0;
            return 1;
        }
        {
            struct timespec slice = {0, 1000000};
            nanosleep(&slice, NULL);
        }
        elapsed++;
    }
}

static int bound_socket_route(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3) {
    int64_t result;
    const int descriptor = (int)(int32_t)a0;
    /* execve keeps the descriptor numbering and drops the close-on-exec ones.
     * Done here and NOT claimed -- the exec itself belongs to whoever handles it
     * -- because a socket's close-on-exec bit lives in this layer's own record
     * and no other sweep can see it. Harmless if the exec then fails: the guest
     * asked for these to be gone the moment it succeeded, and a failed execve
     * that left them open would be the surprising outcome. */
    if (nr == 221 || nr == 281) {
        (void)hl_linux_socket_release_cloexec();
        return 0;
    }
    switch (nr) {
    case 57: /* close */
        if (!hl_linux_socket_is(descriptor)) return 0;
        result = hl_linux_socket_close(descriptor) == 0 ? 0 : -(int64_t)errno;
        break;
    case 63: /* read */
        if (!hl_linux_socket_is(descriptor)) return 0;
        result = bound_socket_transfer(a1, a2, descriptor, 0);
        break;
    case 64: /* write */
        if (!hl_linux_socket_is(descriptor)) return 0;
        result = bound_socket_transfer(a1, a2, descriptor, 1);
        break;
    case 65: /* readv */
        if (!hl_linux_socket_is(descriptor)) return 0;
        result = bound_socket_vector(c, a1, a2, descriptor, 0);
        break;
    case 66: /* writev */
        if (!hl_linux_socket_is(descriptor)) return 0;
        result = bound_socket_vector(c, a1, a2, descriptor, 1);
        break;
    case 23: /* dup */
        if (!hl_linux_socket_is(descriptor)) return 0;
        result = hl_linux_socket_dup(descriptor, -1);
        if (result < 0) result = -(int64_t)errno;
        break;
    case 24: /* dup3, and the legacy dup2 the normalizer folds into it */
        if (!hl_linux_socket_is(descriptor)) return 0;
        if ((int)(int32_t)a1 == descriptor) {
            /* dup2 of a descriptor onto itself is a no-op that must NOT close
             * it; dup3 of the same pair is EINVAL. G_IS_DUP2_COMPAT tells the
             * two apart on the arch where both exist. */
            result = G_IS_DUP2_COMPAT() ? (int64_t)descriptor : -EINVAL;
            break;
        }
        result = hl_linux_socket_dup(descriptor, (int)(int32_t)a1);
        if (result >= 0 && (a2 & (uint64_t)HL_LINUX_O_CLOEXEC) != 0)
            (void)hl_linux_socket_set_cloexec((int)(int32_t)a1, 1);
        if (result < 0) result = -(int64_t)errno;
        break;
    case 25: { /* fcntl */
        uint32_t flags = 0;
        if (!hl_linux_socket_is(descriptor)) return 0;
        if (hl_linux_socket_get_flags(descriptor, &flags) != 0) return 0;
        switch ((int32_t)a1) {
        case HL_LINUX_F_DUPFD:
        case HL_LINUX_F_DUPFD_CLOEXEC:
            result = hl_linux_socket_dup(descriptor, -1);
            if (result >= 0 && (int32_t)a1 == HL_LINUX_F_DUPFD_CLOEXEC)
                (void)hl_linux_socket_set_cloexec((int)result, 1);
            if (result < 0) result = -(int64_t)errno;
            break;
        case HL_LINUX_F_GETFD: result = (flags & HL_LINUX_SOCKET_CLOEXEC) != 0 ? HL_LINUX_FD_CLOEXEC : 0; break;
        case HL_LINUX_F_SETFD:
            result =
                hl_linux_socket_set_cloexec(descriptor, (a2 & HL_LINUX_FD_CLOEXEC) != 0) == 0 ? 0 : -(int64_t)errno;
            break;
        case HL_LINUX_F_GETFL:
            /* A socket is always readable and writable, so the access mode is
             * O_RDWR; the only settable bit this layer records is O_NONBLOCK. */
            result = (int64_t)(uint64_t)(HL_LINUX_O_RDWR |
                                         ((flags & HL_LINUX_SOCKET_NONBLOCK) != 0 ? HL_LINUX_O_NONBLOCK : 0u));
            break;
        case HL_LINUX_F_SETFL:
            result =
                hl_linux_socket_set_nonblock(descriptor, (a2 & HL_LINUX_O_NONBLOCK) != 0) == 0 ? 0 : -(int64_t)errno;
            break;
        default: result = -EINVAL; break;
        }
        break;
    }
    case 29: { /* ioctl */
        uint32_t ready = 0;
        if (!hl_linux_socket_is(descriptor)) return 0;
        if ((uint32_t)a1 == 0x5421u) { /* FIONBIO */
            int requested = 0;
            if (guest_copy_from(&requested, a2, sizeof(requested)) != (ssize_t)sizeof(requested))
                result = -EFAULT;
            else
                result = hl_linux_socket_set_nonblock(descriptor, requested != 0) == 0 ? 0 : -(int64_t)errno;
            break;
        }
        if ((uint32_t)a1 == 0x541bu) { /* FIONREAD */
            uint64_t pending = 0;
            int available;
            if (hl_linux_socket_readiness_and_pending(descriptor, 0, &ready, &pending) != 0) {
                result = -(int64_t)errno;
                break;
            }
            available = pending > (uint64_t)INT_MAX ? INT_MAX : (int)pending;
            result = guest_copy_to(a2, &available, sizeof(available)) == (ssize_t)sizeof(available) ? 0 : -EFAULT;
            break;
        }
        result = -EINVAL;
        break;
    }
    case 73: { /* ppoll */
        struct timespec timeout;
        int64_t milliseconds = -1;
        if (a2 != 0) {
            if (guest_copy_from(&timeout, a2, sizeof(timeout)) != (ssize_t)sizeof(timeout)) return 0;
            milliseconds = (int64_t)timeout.tv_sec * 1000 + timeout.tv_nsec / 1000000;
        }
        if (!bound_socket_poll(c, a0, a1, milliseconds, &result)) return 0;
        break;
    }
    default: return 0;
    }
    (void)a3;
    G_RET(c) = (uint64_t)result;
    return 1;
}
#endif /* _WIN32 */
