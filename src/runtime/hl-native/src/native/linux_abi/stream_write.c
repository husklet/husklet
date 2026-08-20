int64_t hl_linux_pwrite64(hl_linux_abi *linux_abi, hl_linux_fd fd, const void *buffer, size_t size, uint64_t offset) {
    const hl_linux_ofd_entry *found;
    hl_linux_ofd_entry *ofd;
    int64_t result;
    hl_status status;
    if (linux_abi == NULL) return -HL_LINUX_EBADF;
    if (size > (size_t)INT64_MAX) return -HL_LINUX_EINVAL;
    hl_linux_lock(linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, fd, NULL, &found);
    if (status != HL_STATUS_OK) {
        hl_linux_unlock(linux_abi);
        return status == HL_STATUS_NOT_FOUND ? -HL_LINUX_EBADF : hl_linux_error(status);
    }
    ofd = &linux_abi->ofds[(size_t)(found - linux_abi->ofds)];
    if ((ofd->status_flags & HL_LINUX_O_ACCMODE) == HL_LINUX_O_RDONLY) {
        hl_linux_unlock(linux_abi);
        return -HL_LINUX_EBADF;
    }
    ofd->active_operations++;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_lock(linux_abi, ofd);
    result = hl_linux_write_owned(linux_abi, ofd, buffer, size, offset);
    hl_linux_lock(linux_abi);
    ofd->active_operations--;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_unlock(linux_abi, ofd);
    return result;
}

int64_t hl_linux_write(hl_linux_abi *linux_abi, hl_linux_fd fd, const void *buffer, size_t size) {
    const hl_host_file_services *files;
    const hl_linux_fd_entry *fd_entry;
    const hl_linux_ofd_entry *found;
    hl_linux_ofd_entry *ofd;
    int append;
    int64_t result;
    hl_status status;
    if (linux_abi == NULL) return -HL_LINUX_EBADF;
    if (size > (size_t)INT64_MAX) return -HL_LINUX_EINVAL;
    hl_linux_lock(linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, fd, &fd_entry, &found);
    if (status != HL_STATUS_OK) {
        hl_linux_unlock(linux_abi);
        return status == HL_STATUS_NOT_FOUND ? -HL_LINUX_EBADF : hl_linux_error(status);
    }
    ofd = &linux_abi->ofds[fd_entry->ofd];
    if ((ofd->status_flags & HL_LINUX_O_ACCMODE) == HL_LINUX_O_RDONLY) {
        hl_linux_unlock(linux_abi);
        return -HL_LINUX_EBADF;
    }
    append = (ofd->status_flags & HL_LINUX_O_APPEND) != 0;
    ofd->active_operations++;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_lock(linux_abi, ofd);
    files = hl_linux_files(linux_abi);
    if (size != 0 && buffer == NULL)
        result = -HL_LINUX_EINVAL;
    else if (ofd->object_ops != NULL)
        result = ofd->object_ops->write == NULL ? -HL_LINUX_ENOSYS
                                                : ofd->object_ops->write(ofd->object_context, buffer, size);
    else if (files == NULL)
        result = -HL_LINUX_ENOSYS;
    else {
        hl_host_result host_result;
        if (append)
            host_result =
                files->append(linux_abi->host->context, ofd->host_handle, (hl_host_const_bytes){buffer, size});
        else
            host_result = files->write(linux_abi->host->context, ofd->host_handle, buffer, (uint64_t)size);
        result = host_result.status == HL_STATUS_OK ? (int64_t)host_result.value
                                                    : hl_linux_error((hl_status)host_result.status);
        if (host_result.status == HL_STATUS_OK && (host_result.value > size || host_result.value > INT64_MAX))
            result = -HL_LINUX_EIO;
        else if (result > 0 && !append && ofd->offset <= UINT64_MAX - (uint64_t)result)
            ofd->offset += (uint64_t)result;
        else if (result > 0 && append && files->seek != NULL) {
            hl_host_result end = files->seek(linux_abi->host->context, ofd->host_handle, 0, HL_LINUX_SEEK_END);
            if (end.status == HL_STATUS_OK) ofd->offset = end.value;
        }
    }
    hl_linux_lock(linux_abi);
    ofd->active_operations--;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_unlock(linux_abi, ofd);
    return result;
}

static int64_t hl_linux_vector(hl_linux_abi *linux_abi, hl_linux_fd fd, const hl_host_iovec *vectors, uint32_t count,
                               uint64_t offset, uint32_t operation) {
    const hl_linux_fd_entry *fd_entry;
    const hl_linux_ofd_entry *found;
    const hl_host_file_services *files;
    hl_linux_ofd_entry *ofd;
    hl_host_result host_result;
    uint64_t total = 0;
    uint32_t index;
    int writing = operation == 1 || operation == 3;
    int positioned = operation >= 2;
    int append;
    int64_t result;
    hl_status status;
    if (linux_abi == NULL) return -HL_LINUX_EBADF;
    if (count > HL_LINUX_IOV_MAX || (count != 0 && vectors == NULL)) return -HL_LINUX_EINVAL;
    for (index = 0; index < count; ++index) {
        if (vectors[index].size > (uint64_t)INT64_MAX - total ||
            (vectors[index].size != 0 && vectors[index].address == 0))
            return -HL_LINUX_EINVAL;
        total += vectors[index].size;
    }
    hl_linux_lock(linux_abi);
    status = hl_linux_fd_get_unlocked(linux_abi, fd, &fd_entry, &found);
    if (status != HL_STATUS_OK) {
        hl_linux_unlock(linux_abi);
        return status == HL_STATUS_NOT_FOUND ? -HL_LINUX_EBADF : hl_linux_error(status);
    }
    ofd = &linux_abi->ofds[fd_entry->ofd];
    if (!writing && (ofd->status_flags & HL_LINUX_O_ACCMODE) == HL_LINUX_O_WRONLY) {
        hl_linux_unlock(linux_abi);
        return -HL_LINUX_EBADF;
    }
    if (writing && (ofd->status_flags & HL_LINUX_O_ACCMODE) == HL_LINUX_O_RDONLY) {
        hl_linux_unlock(linux_abi);
        return -HL_LINUX_EBADF;
    }
    append = !positioned && writing && (ofd->status_flags & HL_LINUX_O_APPEND) != 0;
    ofd->active_operations++;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_lock(linux_abi, ofd);
    files = hl_linux_files(linux_abi);
    if (files == NULL)
        result = -HL_LINUX_ENOSYS;
    else {
        switch (operation) {
        case 0:
            host_result = files->readv == NULL
                              ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                              : files->readv(linux_abi->host->context, ofd->host_handle, vectors, count);
            break;
        case 1:
            if (append)
                host_result = files->appendv == NULL
                                  ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                                  : files->appendv(linux_abi->host->context, ofd->host_handle, vectors, count);
            else
                host_result = files->writev == NULL
                                  ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                                  : files->writev(linux_abi->host->context, ofd->host_handle, vectors, count);
            break;
        case 2:
            host_result = files->readv_at == NULL
                              ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                              : files->readv_at(linux_abi->host->context, ofd->host_handle, vectors, count, offset);
            break;
        default:
            host_result = files->writev_at == NULL
                              ? (hl_host_result){HL_STATUS_NOT_SUPPORTED, 0, 0, 0}
                              : files->writev_at(linux_abi->host->context, ofd->host_handle, vectors, count, offset);
            break;
        }
        if (host_result.status != HL_STATUS_OK)
            result = hl_linux_error((hl_status)host_result.status);
        else if (host_result.value > total || host_result.value > INT64_MAX)
            result = -HL_LINUX_EIO;
        else
            result = (int64_t)host_result.value;
        if (result > 0 && !positioned && !append && ofd->offset <= UINT64_MAX - (uint64_t)result)
            ofd->offset += (uint64_t)result;
        else if (result > 0 && append && files->seek != NULL) {
            hl_host_result end = files->seek(linux_abi->host->context, ofd->host_handle, 0, HL_LINUX_SEEK_END);
            if (end.status == HL_STATUS_OK) ofd->offset = end.value;
        }
    }
    hl_linux_lock(linux_abi);
    ofd->active_operations--;
    hl_linux_unlock(linux_abi);
    hl_linux_ofd_unlock(linux_abi, ofd);
    return result;
}

int64_t hl_linux_readv(hl_linux_abi *linux_abi, hl_linux_fd fd, const hl_host_iovec *vectors, uint32_t count) {
    return hl_linux_vector(linux_abi, fd, vectors, count, 0, 0);
}

int64_t hl_linux_writev(hl_linux_abi *linux_abi, hl_linux_fd fd, const hl_host_iovec *vectors, uint32_t count) {
    return hl_linux_vector(linux_abi, fd, vectors, count, 0, 1);
}

int64_t hl_linux_preadv(hl_linux_abi *linux_abi, hl_linux_fd fd, const hl_host_iovec *vectors, uint32_t count,
                        uint64_t offset) {
    return hl_linux_vector(linux_abi, fd, vectors, count, offset, 2);
}

int64_t hl_linux_pwritev(hl_linux_abi *linux_abi, hl_linux_fd fd, const hl_host_iovec *vectors, uint32_t count,
                         uint64_t offset) {
    return hl_linux_vector(linux_abi, fd, vectors, count, offset, 3);
}


#if defined(HL_NATIVE_TEST_HOOKS)
#include "../bridge/host.h"

#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>

// ------------------------------------------- F_SETFL(O_APPEND) on an inherited descriptor: behavioral fixture
//
// Drives the REAL hl_linux_fcntl/hl_linux_write pair -- the two syscalls a guest issues -- against a REAL
// guest descriptor table over REAL kernel objects, in the shape a launched process actually has: fd 1 is a
// descriptor the engine ADOPTED from its parent, not one the guest opened.
//
// GNU make sets O_APPEND on stdout before writing (`make --version | cat`), and every adopted descriptor
// reached the appending write path with no appending descriptor established, so the write failed EINVAL and
// make reported "write error: stdout". Linux accepts O_APPEND on a pipe and ignores it, and on a regular
// file makes the write land at EOF; both are pinned below.
struct setfl_append_test_box {
    hl_linux_abi box;
    hl_linux_fd_entry *fds;
    hl_linux_ofd_entry *ofds;
    hl_c_bridge_host *host;
    hl_host_services services;
};

static int setfl_append_test_install(struct setfl_append_test_box *fixture, int native_fd, int guest_fd,
                                     uint32_t status_flags) {
    hl_host_result imported = hl_c_bridge_host_import_file(fixture->host, native_fd, HL_HOST_FILE_WRITE);
    if (imported.status != HL_STATUS_OK) return -1;
    if (hl_linux_fd_install_at(&fixture->box, (hl_linux_fd)guest_fd, imported.value, status_flags, 0) !=
        HL_STATUS_OK) {
        (void)fixture->services.file->close(fixture->services.context, imported.value);
        return -1;
    }
    return 0;
}

HL_API int hl_c_backend_setfl_append_write_test(uint32_t scenario);

HL_API int hl_c_backend_setfl_append_write_test(uint32_t scenario) {
    static const char payload[] = "GNU Make 4.3\n";
    const size_t payload_size = sizeof(payload) - 1;
    struct setfl_append_test_box fixture;
    int pipe_pair[2] = {-1, -1};
    int backing = -1;
    char path[64];
    char observed[32];
    int verdict = 99;
    if (scenario > 2) return -22;
    memset(&fixture, 0, sizeof fixture);
    if (hl_c_bridge_host_create(&fixture.host, &fixture.services) != HL_STATUS_OK) return 10;
    fixture.fds = calloc(HL_LINUX_FD_LIMIT, sizeof(*fixture.fds));
    fixture.ofds = calloc(HL_LINUX_OFD_LIMIT, sizeof(*fixture.ofds));
    if (fixture.fds == NULL || fixture.ofds == NULL ||
        hl_linux_abi_init(&fixture.box, &fixture.services, fixture.fds, HL_LINUX_FD_LIMIT, fixture.ofds,
                          HL_LINUX_OFD_LIMIT) != HL_STATUS_OK) {
        verdict = 11;
        goto release;
    }
    if (scenario == 1) {
        // An adopted stdout redirected to a file the parent opened WITHOUT O_APPEND -- `make --version > log`.
        int written = snprintf(path, sizeof path, "/tmp/hl-setfl-append-%d", (int)getpid());
        if (written < 0 || (size_t)written >= sizeof path) {
            verdict = 12;
            goto release;
        }
        backing = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
        if (backing < 0 || setfl_append_test_install(&fixture, backing, 1, HL_LINUX_O_WRONLY) != 0) {
            verdict = 13;
            goto release;
        }
    } else {
        // An adopted stdout that is a pipe -- `make --version | cat`, and what hl-container hands a launch.
        if (pipe(pipe_pair) != 0 || setfl_append_test_install(&fixture, pipe_pair[1], 1, HL_LINUX_O_WRONLY) != 0) {
            verdict = 13;
            goto release;
        }
    }
    if (scenario == 1) {
        // Establish a position, rewind, and only then ask for O_APPEND: Linux must still write at EOF.
        if (hl_linux_write(&fixture.box, 1, "AAA", 3) != 3 ||
            hl_linux_lseek(&fixture.box, 1, 0, HL_LINUX_SEEK_SET) != 0) {
            verdict = 14;
            goto release;
        }
    }
    if (hl_linux_fcntl(&fixture.box, 1, HL_LINUX_F_SETFL, HL_LINUX_O_WRONLY | HL_LINUX_O_APPEND) != 0) {
        verdict = 15;
        goto release;
    }
    if (scenario == 1) {
        int64_t appended = hl_linux_write(&fixture.box, 1, "BB", 2);
        int reader;
        ssize_t read_back;
        if (appended != 2) {
            verdict = 20;
            goto release;
        }
        reader = open(path, O_RDONLY | O_CLOEXEC);
        read_back = reader < 0 ? -1 : read(reader, observed, sizeof observed);
        if (reader >= 0) close(reader);
        if (read_back != 5 || memcmp(observed, "AAABB", 5) != 0) {
            verdict = 21;
            goto release;
        }
        verdict = 0;
    } else {
        int64_t sent;
        ssize_t read_back;
        if (scenario == 0)
            sent = hl_linux_write(&fixture.box, 1, payload, payload_size);
        else {
            hl_host_iovec vectors[2] = {{(uint64_t)(uintptr_t)payload, 4},
                                        {(uint64_t)(uintptr_t)(payload + 4), payload_size - 4}};
            sent = hl_linux_writev(&fixture.box, 1, vectors, 2);
        }
        if (sent != (int64_t)payload_size) {
            verdict = 20;
            goto release;
        }
        read_back = read(pipe_pair[0], observed, sizeof observed);
        if (read_back != (ssize_t)payload_size || memcmp(observed, payload, payload_size) != 0) {
            verdict = 21;
            goto release;
        }
        verdict = 0;
    }
release:
    if (fixture.fds != NULL && fixture.ofds != NULL) {
        hl_linux_fd fd;
        for (fd = 0; fd < fixture.box.fd_capacity; ++fd) {
            hl_host_handle handle = HL_HOST_HANDLE_INVALID;
            if (hl_linux_fd_close(&fixture.box, fd, &handle) == HL_STATUS_OK && handle != HL_HOST_HANDLE_INVALID)
                (void)fixture.services.file->close(fixture.services.context, handle);
        }
    }
    if (pipe_pair[0] >= 0) close(pipe_pair[0]);
    if (pipe_pair[1] >= 0) close(pipe_pair[1]);
    if (backing >= 0) {
        close(backing);
        unlink(path);
    }
    free(fixture.fds);
    free(fixture.ofds);
    hl_c_bridge_host_destroy(fixture.host);
    return verdict;
}
#endif
