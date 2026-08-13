static int bound_poll_references(uint64_t address, uint64_t count) {
    struct pollfd *fds = NULL;
    uint64_t index;
    hl_linux_fd_snapshot snapshot;
    if (count > SIZE_MAX / sizeof(*fds)) return 0;
    if (count != 0) {
        size_t bytes = (size_t)count * sizeof(*fds);
        fds = malloc(bytes);
        if (fds == NULL || guest_copy_from(fds, address, bytes) != (ssize_t)bytes) {
            free(fds);
            return 0;
        }
    }
    for (index = 0; index < count; ++index)
        if (fds[index].fd >= 0 && bound_snapshot((uint64_t)(unsigned)fds[index].fd, &snapshot)) {
            free(fds);
            return 1;
        }
    free(fds);
    return 0;
}
static int bound_fdsets_reference(uint64_t count, uint64_t read_set, uint64_t write_set, uint64_t except_set) {
    uint64_t fd;
    size_t bytes;
    hl_linux_fd_snapshot snapshot;
    if (count > HL_LINUX_FD_LIMIT) count = HL_LINUX_FD_LIMIT;
    bytes = (size_t)((count + 7u) / 8u);
    uint8_t *sets = calloc(3, bytes == 0 ? 1 : bytes);
    if (sets == NULL) return 0;
    if ((read_set != 0 && guest_copy_from(sets, read_set, bytes) != (ssize_t)bytes) ||
        (write_set != 0 && guest_copy_from(sets + bytes, write_set, bytes) != (ssize_t)bytes) ||
        (except_set != 0 && guest_copy_from(sets + bytes * 2, except_set, bytes) != (ssize_t)bytes)) {
        free(sets);
        return 0;
    }
    for (fd = 0; fd < count; ++fd) {
        uint8_t mask = (uint8_t)(1u << (fd & 7u));
        size_t byte = (size_t)(fd >> 3);
        if (((read_set != 0 && (sets[byte] & mask) != 0) || (write_set != 0 && (sets[bytes + byte] & mask) != 0) ||
             (except_set != 0 && (sets[bytes * 2 + byte] & mask) != 0)) &&
            bound_snapshot(fd, &snapshot)) {
            free(sets);
            return 1;
        }
    }
    free(sets);
    return 0;
}

static uint32_t bound_poll_interests(short events) {
    uint32_t interests = 0;
    if ((events & POLLIN) != 0) interests |= HL_LINUX_READY_READ;
    if ((events & POLLOUT) != 0) interests |= HL_LINUX_READY_WRITE;
    if ((events & POLLPRI) != 0) interests |= HL_LINUX_READY_PRIORITY;
    return interests;
}

static short bound_poll_readiness(uint32_t readiness) {
    short events = 0;
    if ((readiness & HL_LINUX_READY_READ) != 0) events |= POLLIN;
    if ((readiness & HL_LINUX_READY_WRITE) != 0) events |= POLLOUT;
    if ((readiness & HL_LINUX_READY_PRIORITY) != 0) events |= POLLPRI;
    if ((readiness & HL_LINUX_READY_ERROR) != 0) events |= POLLERR;
    if ((readiness & HL_LINUX_READY_HANGUP) != 0) events |= POLLHUP;
    return events;
}

static uint64_t bound_now_ns(void) {
    struct timespec now = {0, 0};
    if (hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now) != 0) return 0;
    return (uint64_t)now.tv_sec * UINT64_C(1000000000) + (uint64_t)now.tv_nsec;
}

static uint64_t bound_deadline(const struct timespec *timeout) {
    uint64_t now;
    uint64_t delta;
    if (timeout == NULL) return UINT64_MAX;
    if (timeout->tv_sec < 0) return 0;
    now = bound_now_ns();
    if ((uint64_t)timeout->tv_sec > UINT64_MAX / UINT64_C(1000000000)) return UINT64_MAX;
    delta = (uint64_t)timeout->tv_sec * UINT64_C(1000000000) + (uint64_t)timeout->tv_nsec;
    return delta > UINT64_MAX - now ? UINT64_MAX : now + delta;
}

/* Poll native descriptors from a private copy: typed guest slots are never host descriptors. */
static int64_t bound_ppoll(struct cpu *c, uint64_t address, uint64_t count, uint64_t timeout_address,
                           uint64_t mask_address) {
    struct pollfd *guest;
    struct timespec timeout_value;
    struct timespec *timeout = timeout_address ? &timeout_value : NULL;
    struct pollfd *native;
    hl_linux_poll_entry *objects;
    uint32_t *object_indices;
    uint64_t deadline;
    uint64_t index;
    uint32_t object_count = 0;
    uint64_t saved = 0;
    uint64_t mask = 0;
    int mask_on;
    int64_t result = 0;
    if (count > (uint64_t)guest_nofile_cur()) return -EINVAL;
    if (count > SIZE_MAX / sizeof(*guest)) return -EFAULT;
    size_t guest_bytes = (size_t)count * sizeof(*guest);
    guest = calloc(count != 0 ? (size_t)count : 1, sizeof(*guest));
    if (!guest) return -ENOMEM;
    if ((count != 0 && guest_copy_from(guest, address, guest_bytes) != (ssize_t)guest_bytes) ||
        (timeout != NULL && guest_copy_from(timeout, timeout_address, sizeof(*timeout)) != sizeof(*timeout))) {
        free(guest);
        return -EFAULT;
    }
    if (timeout != NULL && (timeout->tv_nsec < 0 || timeout->tv_nsec >= 1000000000L)) {
        free(guest);
        return -EINVAL;
    }
    if (mask_address != 0 && (size_t)G_A4(c) != 8) {
        free(guest);
        return -EINVAL;
    }
    if (mask_address != 0 && guest_copy_from(&mask, mask_address, sizeof(mask)) != sizeof(mask)) {
        free(guest);
        return -EFAULT;
    }
    native = calloc(count != 0 ? (size_t)count : 1, sizeof(*native));
    objects = calloc(count != 0 ? (size_t)count : 1, sizeof(*objects));
    object_indices = calloc(count != 0 ? (size_t)count : 1, sizeof(*object_indices));
    if (native == NULL || objects == NULL || object_indices == NULL) {
        free(native);
        free(objects);
        free(object_indices);
        free(guest);
        return -ENOMEM;
    }
    memcpy(native, guest, (size_t)count * sizeof(*native));
    for (index = 0; index < count; ++index) {
        hl_linux_fd_snapshot snapshot;
        guest[index].revents = 0;
        if (guest[index].fd >= 0 && bound_snapshot((uint64_t)(unsigned)guest[index].fd, &snapshot)) {
            object_indices[object_count] = (uint32_t)index;
            objects[object_count++] = (hl_linux_poll_entry){snapshot.fd, bound_poll_interests(guest[index].events), 0};
            native[index].fd = -1;
        }
    }
    deadline = bound_deadline(timeout);
    mask_on = poll_sigmask_enter(c, mask_address != 0, mask, &saved);
    for (;;) {
        int native_ready;
        int64_t object_ready = hl_linux_object_poll(g_linux_box, objects, object_count, 0);
        int wait_ms = 0;
        uint64_t now = bound_now_ns();
        if (object_ready < 0) {
            result = object_ready;
            break;
        }
        if (object_ready == 0 && deadline != 0 && now < deadline) wait_ms = 1;
        native_ready = poll(native, (nfds_t)count, wait_ms);
        if (native_ready < 0) {
            if (svc_poll_retry(c)) continue;
            result = -errno;
            break;
        }
        if (object_ready != 0 || native_ready != 0 || deadline == 0 ||
            (deadline != UINT64_MAX && bound_now_ns() >= deadline)) {
            result = native_ready + object_ready;
            for (index = 0; index < count; ++index)
                guest[index].revents = native[index].revents;
            for (index = 0; index < object_count; ++index)
                guest[object_indices[index]].revents = bound_poll_readiness(objects[index].readiness);
            break;
        }
    }
    if (mask_on) poll_sigmask_leave(c, saved);
    if (result >= 0 && timeout != NULL) {
        uint64_t now = bound_now_ns();
        uint64_t left = deadline != UINT64_MAX && deadline > now ? deadline - now : 0;
        timeout->tv_sec = (time_t)(left / UINT64_C(1000000000));
        timeout->tv_nsec = (long)(left % UINT64_C(1000000000));
    }
    if (result >= 0 &&
        ((count != 0 && guest_copy_to(address, guest, guest_bytes) != (ssize_t)guest_bytes) ||
         (timeout != NULL && guest_copy_to(timeout_address, timeout, sizeof(*timeout)) != sizeof(*timeout))))
        result = -EFAULT;
    free(objects);
    free(object_indices);
    free(native);
    free(guest);
    return result;
}

static int bound_set_test(const uint8_t *set, uint32_t fd) {
    return set != NULL && (set[fd >> 3] & (uint8_t)(1u << (fd & 7u))) != 0;
}

static void bound_set_mark(uint8_t *set, uint32_t fd) {
    if (set != NULL) set[fd >> 3] |= (uint8_t)(1u << (fd & 7u));
}

static int64_t bound_pselect(struct cpu *c, uint64_t count_value, uint64_t read_address, uint64_t write_address,
                             uint64_t except_address) {
    uint32_t count = count_value > HL_LINUX_FD_LIMIT ? HL_LINUX_FD_LIMIT : (uint32_t)count_value;
    size_t bytes = ((size_t)count + 7u) / 8u;
    uint8_t *guest_read = NULL;
    uint8_t *guest_write = NULL;
    uint8_t *guest_except = NULL;
    uint8_t *sets = NULL;
    struct timespec timeout_value;
    struct timespec *timeout = G_A4(c) ? &timeout_value : NULL;
    uint64_t mask_pair_address = G_A5(c);
    uint8_t *requested;
    struct pollfd *native;
    hl_linux_poll_entry *objects;
    uint32_t *object_indices;
    uint32_t object_count = 0;
    uint32_t fd;
    uint64_t deadline;
    uint64_t mask_address = 0;
    uint64_t saved = 0;
    uint64_t mask = 0;
    int mask_on;
    int64_t result = 0;
    if (count_value > INT_MAX) return -EINVAL;
    sets = calloc(bytes != 0 ? bytes * 3 : 1, 1);
    if (!sets) return -ENOMEM;
    if (read_address) guest_read = sets;
    if (write_address) guest_write = sets + bytes;
    if (except_address) guest_except = sets + bytes * 2;
    if ((guest_read && guest_copy_from(guest_read, read_address, bytes) != (ssize_t)bytes) ||
        (guest_write && guest_copy_from(guest_write, write_address, bytes) != (ssize_t)bytes) ||
        (guest_except && guest_copy_from(guest_except, except_address, bytes) != (ssize_t)bytes) ||
        (timeout && guest_copy_from(timeout, G_A4(c), sizeof(*timeout)) != sizeof(*timeout))) {
        free(sets);
        return -EFAULT;
    }
    if (timeout != NULL && (timeout->tv_nsec < 0 || timeout->tv_nsec >= 1000000000L)) {
        free(sets);
        return -EINVAL;
    }
    if (mask_pair_address != 0) {
        uint64_t pair[2];
        if (guest_copy_from(pair, mask_pair_address, sizeof(pair)) != sizeof(pair)) {
            free(sets);
            return -EFAULT;
        }
        if (pair[0] != 0) {
            if (pair[1] != 8) {
                free(sets);
                return -EINVAL;
            }
            if (guest_copy_from(&mask, pair[0], sizeof(mask)) != sizeof(mask)) {
                free(sets);
                return -EFAULT;
            }
            mask_address = pair[0];
        }
    }
    requested = calloc(bytes != 0 ? bytes * 3 : 1, 1);
    native = calloc(count != 0 ? count : 1, sizeof(*native));
    objects = calloc(count != 0 ? count : 1, sizeof(*objects));
    object_indices = calloc(count != 0 ? count : 1, sizeof(*object_indices));
    if (requested == NULL || native == NULL || objects == NULL || object_indices == NULL) {
        result = -ENOMEM;
        goto done;
    }
    if (guest_read != NULL) memcpy(requested, guest_read, bytes);
    if (guest_write != NULL) memcpy(requested + bytes, guest_write, bytes);
    if (guest_except != NULL) memcpy(requested + bytes * 2, guest_except, bytes);
    for (fd = 0; fd < count; ++fd) {
        uint32_t interests = 0;
        hl_linux_fd_snapshot snapshot;
        if (bound_set_test(requested, fd)) interests |= HL_LINUX_READY_READ;
        if (bound_set_test(requested + bytes, fd)) interests |= HL_LINUX_READY_WRITE;
        if (bound_set_test(requested + bytes * 2, fd)) interests |= HL_LINUX_READY_PRIORITY;
        native[fd] = (struct pollfd){.fd = interests != 0 ? (int)fd : -1, .events = bound_poll_readiness(interests)};
        if (interests != 0 && bound_snapshot(fd, &snapshot)) {
            object_indices[object_count] = fd;
            objects[object_count++] = (hl_linux_poll_entry){snapshot.fd, interests, 0};
            native[fd].fd = -1;
        }
    }
    deadline = bound_deadline(timeout);
    mask_on = poll_sigmask_enter(c, mask_address != 0, mask, &saved);
    for (;;) {
        int native_ready;
        int64_t object_ready = hl_linux_object_poll(g_linux_box, objects, object_count, 0);
        uint64_t now = bound_now_ns();
        if (object_ready < 0) {
            result = object_ready;
            break;
        }
        native_ready = poll(native, count, object_ready == 0 && deadline != 0 && now < deadline ? 1 : 0);
        if (native_ready < 0) {
            if (svc_poll_retry(c)) continue;
            result = -errno;
            break;
        }
        for (fd = 0; fd < count; ++fd)
            if ((native[fd].revents & POLLNVAL) != 0) {
                result = -EBADF;
                goto waited;
            }
        if (native_ready != 0 || object_ready != 0 || deadline == 0 ||
            (deadline != UINT64_MAX && bound_now_ns() >= deadline)) {
            if (guest_read != NULL) memset(guest_read, 0, bytes);
            if (guest_write != NULL) memset(guest_write, 0, bytes);
            if (guest_except != NULL) memset(guest_except, 0, bytes);
            result = 0;
            for (fd = 0; fd < count; ++fd) {
                int ready = 0;
                if ((native[fd].revents & (POLLIN | POLLHUP | POLLERR)) != 0 && bound_set_test(requested, fd)) {
                    bound_set_mark(guest_read, fd);
                    ready = 1;
                }
                if ((native[fd].revents & (POLLOUT | POLLERR)) != 0 && bound_set_test(requested + bytes, fd)) {
                    bound_set_mark(guest_write, fd);
                    ready = 1;
                }
                if ((native[fd].revents & POLLPRI) != 0 && bound_set_test(requested + bytes * 2, fd)) {
                    bound_set_mark(guest_except, fd);
                    ready = 1;
                }
                result += ready;
            }
            for (fd = 0; fd < object_count; ++fd) {
                uint32_t descriptor = object_indices[fd];
                int ready = 0;
                if ((objects[fd].readiness & (HL_LINUX_READY_READ | HL_LINUX_READY_HANGUP | HL_LINUX_READY_ERROR)) !=
                        0 &&
                    bound_set_test(requested, descriptor)) {
                    bound_set_mark(guest_read, descriptor);
                    ready = 1;
                }
                if ((objects[fd].readiness & (HL_LINUX_READY_WRITE | HL_LINUX_READY_ERROR)) != 0 &&
                    bound_set_test(requested + bytes, descriptor)) {
                    bound_set_mark(guest_write, descriptor);
                    ready = 1;
                }
                if ((objects[fd].readiness & HL_LINUX_READY_PRIORITY) != 0 &&
                    bound_set_test(requested + bytes * 2, descriptor)) {
                    bound_set_mark(guest_except, descriptor);
                    ready = 1;
                }
                result += ready;
            }
            break;
        }
    }
waited:
    if (mask_on) poll_sigmask_leave(c, saved);
    if (result >= 0 && timeout != NULL) {
        uint64_t now = bound_now_ns();
        uint64_t left = deadline != UINT64_MAX && deadline > now ? deadline - now : 0;
        timeout->tv_sec = (time_t)(left / UINT64_C(1000000000));
        timeout->tv_nsec = (long)(left % UINT64_C(1000000000));
    }
done:
    if (result >= 0 && ((guest_read && guest_copy_to(read_address, guest_read, bytes) != (ssize_t)bytes) ||
                        (guest_write && guest_copy_to(write_address, guest_write, bytes) != (ssize_t)bytes) ||
                        (guest_except && guest_copy_to(except_address, guest_except, bytes) != (ssize_t)bytes) ||
                        (timeout && guest_copy_to(G_A4(c), timeout, sizeof(*timeout)) != sizeof(*timeout))))
        result = -EFAULT;
    free(object_indices);
    free(objects);
    free(native);
    free(requested);
    free(sets);
    return result;
}

/* Return 1 with a scoped native alias for a typed file, 0 for an already-native fd, or -errno. */
static int bound_attachment_borrow(int guest_fd, int *native_fd) {
    hl_linux_fd_snapshot snapshot;
    const hl_host_posix_attachment_services *attachments;
    hl_host_result borrowed;
    if (native_fd == NULL || guest_fd < 0) return -EBADF;
    if (!bound_snapshot((uint64_t)(uint32_t)guest_fd, &snapshot)) {
        if (fcntl(guest_fd, F_GETFD) < 0) return -EBADF;
        *native_fd = guest_fd;
        return 0;
    }
    attachments = g_host_services == NULL ? NULL : g_host_services->posix_attachment;
    if (attachments == NULL || attachments->abi != HL_HOST_POSIX_ATTACHMENT_ABI ||
        attachments->size < sizeof(*attachments) || attachments->borrow_file == NULL)
        return -EOPNOTSUPP;
    borrowed = attachments->borrow_file(g_host_services->context, snapshot.host_handle);
    if (borrowed.status != HL_STATUS_OK) return bound_host_error(borrowed.status);
    if (borrowed.value > INT_MAX) {
        if (attachments->release != NULL) (void)attachments->release(g_host_services->context, borrowed.value);
        return -EIO;
    }
    *native_fd = (int)borrowed.value;
    return 1;
}

static void bound_attachment_release(int native_fd) {
    const hl_host_posix_attachment_services *attachments =
        g_host_services == NULL ? NULL : g_host_services->posix_attachment;
    if (attachments != NULL && attachments->release != NULL)
        (void)attachments->release(g_host_services->context, (uint64_t)(unsigned)native_fd);
    else
        close(native_fd);
}

static int64_t bound_stream_read(const hl_linux_fd_snapshot *file, int native_fd, void *buffer, size_t size,
                                 off_t *offset) {
    if (file != NULL)
        return offset != NULL ? hl_linux_pread64(g_linux_box, file->fd, buffer, size, (uint64_t)*offset)
                              : hl_linux_read(g_linux_box, file->fd, buffer, size);
    ssize_t count = offset != NULL ? pread(native_fd, buffer, size, *offset) : read(native_fd, buffer, size);
    return count < 0 ? -errno : count;
}

static int64_t bound_stream_write(const hl_linux_fd_snapshot *file, int native_fd, const void *buffer, size_t size,
                                  off_t *offset) {
    if (file != NULL)
        return offset != NULL ? hl_linux_pwrite64(g_linux_box, file->fd, buffer, size, (uint64_t)*offset)
                              : hl_linux_write(g_linux_box, file->fd, buffer, size);
    ssize_t count = offset != NULL ? pwrite(native_fd, buffer, size, *offset) : write(native_fd, buffer, size);
    return count < 0 ? -errno : count;
}

static int64_t bound_guest_read(const hl_linux_fd_snapshot *file, uint64_t guest, size_t size, uint64_t offset,
                                int positioned) {
    if (size == 0)
        return positioned ? hl_linux_pread64(g_linux_box, file->fd, NULL, 0, offset)
                          : hl_linux_read(g_linux_box, file->fd, NULL, 0);
    size_t accessible = guest_accessible_prefix(guest, size, HL_LOGICAL_VMA_WRITE);
    if (accessible == 0) return bound_read_no_copy(file, offset, positioned);
    void *buffer = malloc(accessible);
    if (buffer == NULL) return -ENOMEM;
    int64_t result = positioned ? hl_linux_pread64(g_linux_box, file->fd, buffer, accessible, offset)
                                : hl_linux_read(g_linux_box, file->fd, buffer, accessible);
    if (result > 0) {
        ssize_t copied = guest_copy_to(guest, buffer, (size_t)result);
        if (copied != result) result = copied > 0 ? copied : -EFAULT;
    }
    free(buffer);
    return result;
}

static int64_t bound_guest_write(const hl_linux_fd_snapshot *file, uint64_t guest, size_t size, uint64_t offset,
                                 int positioned) {
    if (size == 0)
        return positioned ? hl_linux_pwrite64(g_linux_box, file->fd, NULL, 0, offset)
                          : hl_linux_write(g_linux_box, file->fd, NULL, 0);
    if (bound_access_rejects(file, 0)) return -EBADF;
    void *buffer = malloc(size);
    if (buffer == NULL) return -ENOMEM;
    ssize_t copied = guest_copy_from(buffer, guest, size);
    if (copied <= 0) {
        free(buffer);
        return -EFAULT;
    }
    int64_t result = positioned ? hl_linux_pwrite64(g_linux_box, file->fd, buffer, (size_t)copied, offset)
                                : hl_linux_write(g_linux_box, file->fd, buffer, (size_t)copied);
    free(buffer);
    return result;
}

static int64_t bound_sendfile(const hl_linux_fd_snapshot *output, int output_fd, const hl_linux_fd_snapshot *input,
                              int input_fd, uint64_t offset_address, uint64_t count) {
    off_t supplied_offset = 0;
    off_t *input_offset = NULL;
    uint64_t done = 0;
    int64_t error = 0;
    char buffer[8192];
    if (input == NULL) {
        struct stat metadata;
        if (fstat(input_fd, &metadata) != 0) return -errno;
        if (!S_ISREG(metadata.st_mode)) return -EINVAL;
    } else if (g_host_services != NULL && g_host_services->file != NULL && g_host_services->file->metadata != NULL) {
        hl_host_file_metadata metadata;
        hl_host_result status =
            g_host_services->file->metadata(g_host_services->context, input->host_handle, &metadata);
        if (status.status != HL_STATUS_OK) return bound_host_error(status.status);
        if (metadata.type != HL_HOST_FILE_TYPE_REGULAR) return -EINVAL;
    }
    if (offset_address != 0) {
        if (guest_copy_from(&supplied_offset, offset_address, sizeof(supplied_offset)) !=
            (ssize_t)sizeof(supplied_offset))
            return -EFAULT;
        if (supplied_offset < 0) return -EINVAL;
        input_offset = &supplied_offset;
    }
    if (count > UINT64_C(0x7ffff000)) count = UINT64_C(0x7ffff000); /* Linux MAX_RW_COUNT */
    while (done < count) {
        uint64_t remaining = count - done;
        size_t chunk = remaining < sizeof(buffer) ? (size_t)remaining : sizeof(buffer);
        int64_t read_count = bound_stream_read(input, input_fd, buffer, chunk, input_offset);
        if (read_count <= 0) {
            error = read_count;
            break;
        }
        int64_t written = bound_stream_write(output, output_fd, buffer, (size_t)read_count, NULL);
        if (written <= 0) {
            error = written;
            if (input_offset == NULL)
                (void)(input != NULL ? hl_linux_lseek(g_linux_box, input->fd, -read_count, SEEK_CUR)
                                     : lseek(input_fd, (off_t)-read_count, SEEK_CUR));
            break;
        }
        if (input_offset != NULL) *input_offset += (off_t)written;
        if (output != NULL) bound_mapping_file_written(output, output->offset + done, (uint64_t)written);
        done += (uint64_t)written;
        if (written != read_count) {
            if (input_offset == NULL)
                (void)(input != NULL ? hl_linux_lseek(g_linux_box, input->fd, written - read_count, SEEK_CUR)
                                     : lseek(input_fd, (off_t)(written - read_count), SEEK_CUR));
            break;
        }
    }
    if (offset_address != 0 &&
        guest_copy_to(offset_address, &supplied_offset, sizeof(supplied_offset)) != (ssize_t)sizeof(supplied_offset))
        return done != 0 ? (int64_t)done : -EFAULT;
    return done != 0 ? (int64_t)done : error;
}
