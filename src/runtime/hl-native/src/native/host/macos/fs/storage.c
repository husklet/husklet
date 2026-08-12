static hl_host_result hl_macos_file_append(void *context, hl_host_handle file, hl_host_const_bytes input) {
    int descriptor = hl_macos_file_descriptor(context, file, 1);
    ssize_t count;
    if ((input.size != 0 && input.data == NULL) || descriptor < 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = write(descriptor, input.data, input.size);
    if (count < 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0);
}

static hl_host_result hl_macos_file_vector(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                           uint32_t count, uint64_t offset, int operation) {
    struct iovec native[HL_HOST_FILE_IOV_MAX];
    int descriptor = hl_macos_file_descriptor(context, file, operation == 4);
    ssize_t transferred;
    uint32_t index;
    if ((count != 0 && vectors == NULL) || count > HL_HOST_FILE_IOV_MAX || descriptor < 0 ||
        ((operation == 2 || operation == 3) && offset > INT64_MAX))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    for (index = 0; index < count; ++index) {
        if (vectors[index].size > SIZE_MAX || vectors[index].address > UINTPTR_MAX)
            return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        native[index].iov_base = (void *)(uintptr_t)vectors[index].address;
        native[index].iov_len = (size_t)vectors[index].size;
    }
    /*
     * An empty vector transfers nothing and succeeds on Linux, but Darwin's readv/writev family
     * rejects iovcnt 0 outright with EINVAL. Present one zero-length segment instead of zero
     * segments so the descriptor's access-mode check still runs and the caller observes the Linux
     * result. The Linux backend needs no such adjustment.
     */
    if (count == 0) {
        static char empty_segment;
        native[0].iov_base = &empty_segment;
        native[0].iov_len = 0;
        count = 1;
    }
    switch (operation) {
    case 0: transferred = readv(descriptor, native, (int)count); break;
    case 1: transferred = writev(descriptor, native, (int)count); break;
    case 2: transferred = preadv(descriptor, native, (int)count, (off_t)offset); break;
    case 3: transferred = pwritev(descriptor, native, (int)count, (off_t)offset); break;
    default: transferred = writev(descriptor, native, (int)count); break;
    }
    if (transferred < 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, (uint64_t)transferred, 0);
}

#define HL_MACOS_VECTOR_WRAPPER(name, operation)                                                                       \
    static hl_host_result name(void *context, hl_host_handle file, const hl_host_iovec *vectors, uint32_t count) {     \
        return hl_macos_file_vector(context, file, vectors, count, 0, operation);                                      \
    }
HL_MACOS_VECTOR_WRAPPER(hl_macos_file_readv, 0)
HL_MACOS_VECTOR_WRAPPER(hl_macos_file_writev, 1)
HL_MACOS_VECTOR_WRAPPER(hl_macos_file_appendv, 4)

static hl_host_result hl_macos_file_truncate(void *context, hl_host_handle file, uint64_t size) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    if (descriptor < 0 || size > INT64_MAX) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return ftruncate(descriptor, (off_t)size) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_file_sync(void *context, hl_host_handle file) {
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return fsync(descriptor) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_file_sync_range(void *context, hl_host_handle file, uint64_t offset, uint64_t size,
                                               uint32_t flags) {
    if ((flags & ~7u) != 0 || offset > INT64_MAX || size > INT64_MAX || offset > INT64_MAX - size)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* macOS has no range durability primitive; fsync is stronger and truthful. */
    return hl_macos_file_sync(context, file);
}

static hl_host_result hl_macos_file_sync_filesystem(void *context, hl_host_handle file) {
    /* fsync the selected filesystem object; macOS exposes no syncfs equivalent. */
    return hl_macos_file_sync(context, file);
}

static int hl_macos_write_zeros(int descriptor, off_t begin, off_t end) {
    static const unsigned char zeros[65536];
    while (begin < end) {
        size_t request = (uint64_t)(end - begin) < sizeof(zeros) ? (size_t)(end - begin) : sizeof(zeros);
        ssize_t count = pwrite(descriptor, zeros, request, begin);
        if (count < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (count == 0) {
            errno = EIO;
            return -1;
        }
        begin += count;
    }
    return 0;
}

static hl_host_result hl_macos_file_allocate_range(void *context, hl_host_handle file, uint32_t mode, uint64_t offset,
                                                   uint64_t size) {
    const uint32_t keep = HL_HOST_FILE_ALLOC_KEEP_SIZE;
    const uint32_t punch = HL_HOST_FILE_ALLOC_PUNCH_HOLE;
    const uint32_t collapse = HL_HOST_FILE_ALLOC_COLLAPSE_RANGE;
    const uint32_t zero = HL_HOST_FILE_ALLOC_ZERO_RANGE;
    const uint32_t insert = HL_HOST_FILE_ALLOC_INSERT_RANGE;
    const uint32_t unshare = HL_HOST_FILE_ALLOC_UNSHARE_RANGE;
    const uint32_t allowed = keep | punch | collapse | zero | insert | unshare;
    unsigned char buffer[65536];
    struct stat status;
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    off_t begin, length, end, current;
    if (descriptor < 0 || size == 0 || offset > INT64_MAX || size > INT64_MAX || offset > INT64_MAX - size ||
        (mode & ~allowed) != 0 || ((mode & punch) != 0 && (mode & keep) == 0) ||
        ((mode & collapse) != 0 && mode != collapse) || ((mode & insert) != 0 && mode != insert) ||
        ((mode & unshare) != 0 && (mode & ~(unshare | keep)) != 0))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    begin = (off_t)offset;
    length = (off_t)size;
    end = begin + length;
    if (fstat(descriptor, &status) != 0) return hl_macos_errno();
    if (!S_ISREG(status.st_mode)) return hl_macos_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    current = status.st_size;
    if ((mode & punch) != 0) {
#ifdef F_PUNCHHOLE
        struct fpunchhole hole = {.fp_offset = begin, .fp_length = length};
        if (fcntl(descriptor, F_PUNCHHOLE, &hole) == 0) return hl_macos_result(HL_STATUS_OK, 0, 0);
        if (errno != EINVAL) return hl_macos_errno();
#endif
        if (begin < current && hl_macos_write_zeros(descriptor, begin, end < current ? end : current) != 0)
            return hl_macos_errno();
        return hl_macos_result(HL_STATUS_OK, 0, 0);
    }
    if ((mode & zero) != 0) {
        off_t zero_end = (mode & keep) != 0 && end > current ? current : end;
        if ((mode & keep) == 0 && end > current && ftruncate(descriptor, end) != 0) return hl_macos_errno();
        if (begin < zero_end && hl_macos_write_zeros(descriptor, begin, zero_end) != 0) return hl_macos_errno();
        return hl_macos_result(HL_STATUS_OK, 0, 0);
    }
    if ((mode & collapse) != 0) {
        off_t read_position = end, write_position = begin;
        if (end >= current) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        while (read_position < current) {
            size_t request = (uint64_t)(current - read_position) < sizeof(buffer) ? (size_t)(current - read_position)
                                                                                  : sizeof(buffer);
            ssize_t count = pread(descriptor, buffer, request, read_position);
            if (count < 0 && errno == EINTR) continue;
            if (count <= 0) {
                if (count == 0) errno = EIO;
                return hl_macos_errno();
            }
            size_t done = 0;
            while (done < (size_t)count) {
                ssize_t written = pwrite(descriptor, buffer + done, (size_t)count - done, write_position + (off_t)done);
                if (written < 0 && errno == EINTR) continue;
                if (written <= 0) {
                    if (written == 0) errno = EIO;
                    return hl_macos_errno();
                }
                done += (size_t)written;
            }
            read_position += count;
            write_position += count;
        }
        return ftruncate(descriptor, current - length) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
    }
    if ((mode & insert) != 0) {
        if (begin >= current || current > INT64_MAX - length) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
        if (ftruncate(descriptor, current + length) != 0) return hl_macos_errno();
        for (off_t remaining = current - begin; remaining != 0;) {
            size_t request = (uint64_t)remaining < sizeof(buffer) ? (size_t)remaining : sizeof(buffer);
            off_t source = begin + remaining - (off_t)request;
            ssize_t count = pread(descriptor, buffer, request, source);
            if (count < 0 && errno == EINTR) continue;
            if (count <= 0) {
                if (count == 0) errno = EIO;
                return hl_macos_errno();
            }
            size_t done = 0;
            while (done < (size_t)count) {
                ssize_t written =
                    pwrite(descriptor, buffer + done, (size_t)count - done, source + length + (off_t)done);
                if (written < 0 && errno == EINTR) continue;
                if (written <= 0) {
                    if (written == 0) errno = EIO;
                    return hl_macos_errno();
                }
                done += (size_t)written;
            }
            remaining -= count;
        }
        if (hl_macos_write_zeros(descriptor, begin, end) != 0) return hl_macos_errno();
        return hl_macos_result(HL_STATUS_OK, 0, 0);
    }
#ifdef F_PREALLOCATE
    {
        fstore_t store = {.fst_flags = F_ALLOCATECONTIG,
                          .fst_posmode = F_PEOFPOSMODE,
                          .fst_offset = begin - current,
                          .fst_length = length,
                          .fst_bytesalloc = 0};
        if (fcntl(descriptor, F_PREALLOCATE, &store) != 0) {
            store.fst_flags = F_ALLOCATEALL;
            if (fcntl(descriptor, F_PREALLOCATE, &store) != 0 && errno != EINVAL && errno != ENOTSUP)
                return hl_macos_errno();
        }
    }
#endif
    if ((mode & keep) == 0 && end > current && ftruncate(descriptor, end) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_file_filesystem_metadata(void *context, hl_host_handle file,
                                                        hl_host_filesystem_metadata *output) {
    struct statfs status;
    int descriptor = hl_macos_file_descriptor(context, file, 0);
    if (descriptor < 0 || output == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (fstatfs(descriptor, &status) != 0) return hl_macos_errno();
    memset(output, 0, sizeof(*output));
    output->blocks = status.f_blocks;
    output->blocks_free = status.f_bfree;
    output->blocks_available = status.f_bavail;
    output->files = status.f_files;
    output->files_free = status.f_ffree;
    output->filesystem_id[0] = (uint32_t)status.f_fsid.val[0];
    output->filesystem_id[1] = (uint32_t)status.f_fsid.val[1];
    output->block_size = (uint64_t)status.f_bsize;
    output->fragment_size = (uint64_t)status.f_bsize;
    output->name_max = NAME_MAX;
    output->flags = (uint64_t)status.f_flags;
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_file_read_directory(void *context, hl_host_handle file, hl_host_file_entry *entries,
                                                   uint32_t entry_capacity, uint32_t byte_capacity) {
    hl_host_macos *host = context;
    uint32_t produced = 0, used = 0;
    int saved_error = 0;
    if (entries == NULL || entry_capacity == 0 || byte_capacity < 24 || byte_capacity > UINT32_C(1 << 20))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    hl_macos_file *entry = hl_macos_file_lookup(host, file);
    if (entry == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (entry->directory_shared == NULL) {
        struct stat status;
        if (fstat(entry->descriptor, &status) != 0) {
            saved_error = errno;
        } else if (!S_ISDIR(status.st_mode)) {
            saved_error = ENOTDIR;
        } else {
            entry->directory_shared = hl_macos_directory_shared_create();
            if (entry->directory_shared == NULL) saved_error = ENOMEM;
        }
    }
    hl_macos_directory_shared *shared = entry->directory_shared;
    if (saved_error == 0 && shared == NULL) saved_error = EIO;
    if (shared != NULL) pthread_mutex_lock(&shared->lock);
    if (saved_error == 0 && entry->directory == NULL) {
        int duplicate = fcntl(entry->descriptor, F_DUPFD_CLOEXEC, 0);
        if (duplicate < 0) {
            saved_error = errno;
        } else {
            entry->directory = fdopendir(duplicate);
            if (entry->directory == NULL) {
                saved_error = errno;
                close(duplicate);
            } else {
                entry->directory_position = 0;
            }
        }
    }
    if (saved_error == 0 && entry->directory == NULL) saved_error = EIO;
    if (saved_error == 0 && entry->directory_position != shared->position) {
        rewinddir(entry->directory);
        entry->directory_position = 0;
        while (entry->directory_position < shared->position) {
            errno = 0;
            if (readdir(entry->directory) == NULL) {
                saved_error = errno != 0 ? errno : EINVAL;
                break;
            }
            entry->directory_position++;
        }
    }
    while (saved_error == 0 && produced < entry_capacity) {
        long before = telldir(entry->directory);
        errno = 0;
        struct dirent *native = readdir(entry->directory);
        if (native == NULL) {
            saved_error = errno;
            break;
        }
        size_t name_size = strnlen(native->d_name, sizeof(native->d_name));
        uint32_t record_size = (uint32_t)((19u + name_size + 1u + 7u) & ~(size_t)7u);
        if (name_size == sizeof(native->d_name)) {
            saved_error = EIO;
            break;
        }
        if (record_size > byte_capacity - used) {
            seekdir(entry->directory, before);
            if (produced == 0) saved_error = EINVAL;
            break;
        }
        entries[produced].object = native->d_ino;
        entries[produced].next_offset = entry->directory_position + 1;
        entries[produced].type = native->d_type;
        entries[produced].name_size = (uint32_t)name_size;
        memcpy(entries[produced].name, native->d_name, name_size + 1);
        used += record_size;
        produced++;
        entry->directory_position++;
    }
    if (saved_error == 0) shared->position = entry->directory_position;
    if (shared != NULL) pthread_mutex_unlock(&shared->lock);
    pthread_mutex_unlock(&host->lock);
    if (saved_error != 0) {
        errno = saved_error;
        return hl_macos_errno();
    }
    return hl_macos_result(HL_STATUS_OK, produced, used);
}
