/*
 * Windows scalar, positioned, append, and vectored file transfers.
 *
 * This file is included by file.c so the service entry points and their shared
 * helpers remain translation-unit private.
 */
/* --- reads and writes ------------------------------------------------------- */

static ULONG hl_windows_transfer_size(uint64_t size) {
    return size > HL_WINDOWS_IO_MAX ? (ULONG)HL_WINDOWS_IO_MAX : (ULONG)size;
}

/*
 * A positioned transfer. The save/restore bracket is what makes this pread and
 * pwrite rather than "seek then read": on a synchronous file object NtReadFile
 * updates CurrentByteOffset even when an explicit offset is supplied. It is
 * taken under the host lock so that two positioned calls on one handle cannot
 * interleave their save and restore; sequential read/write take no lock at all,
 * because the kernel already serialises those on the file object.
 */
static hl_host_result hl_windows_file_positioned(hl_host_windows *host, hl_host_handle file, uint64_t offset,
                                                 void *buffer, uint64_t size, int writing) {
    hl_windows_handle_entry *entry;
    FILE_POSITION_INFORMATION saved;
    IO_STATUS_BLOCK status_block;
    LARGE_INTEGER where;
    NTSTATUS status;
    HANDLE object;
    uint64_t moved;
    int restore;
    if (offset > (uint64_t)INT64_MAX) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    where.QuadPart = (LONGLONG)offset;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, file, HL_WINDOWS_HANDLE_FILE);
    if (entry == NULL) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    object = entry->object;
    restore = host->nt.query_information_file(object, &status_block, &saved, (ULONG)sizeof(saved),
                                              FilePositionInformation) == HL_NT_SUCCESS;
    status = writing ? host->nt.write_file(object, NULL, NULL, NULL, &status_block, buffer,
                                           hl_windows_transfer_size(size), &where, NULL)
                     : host->nt.read_file(object, NULL, NULL, NULL, &status_block, buffer,
                                          hl_windows_transfer_size(size), &where, NULL);
    /* Read the transferred count before the restore reuses the status block. */
    moved = (uint64_t)status_block.Information;
    if (restore) {
        IO_STATUS_BLOCK restored;
        (void)host->nt.set_information_file(object, &restored, &saved, (ULONG)sizeof(saved), FilePositionInformation);
    }
    hl_windows_unlock(host);
    if (status == HL_NT_END_OF_FILE) return hl_windows_result(HL_STATUS_OK, 0, 0);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    return hl_windows_result(HL_STATUS_OK, moved, 0);
}

static hl_host_result hl_windows_file_read_at(void *context, hl_host_handle file, uint64_t offset,
                                              hl_host_bytes output) {
    if (output.size != 0 && output.data == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_windows_file_positioned(context, file, offset, output.data, output.size, 0);
}

static hl_host_result hl_windows_file_write_at(void *context, hl_host_handle file, uint64_t offset,
                                               hl_host_const_bytes input) {
    if (input.size != 0 && input.data == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* The cast drops const only to reach one call site; NtWriteFile does not
     * write through it. */
    return hl_windows_file_positioned(context, file, offset, (void *)(size_t)input.data, input.size, 1);
}

/* A sequential transfer against the file object's own position. */
static hl_host_result hl_windows_file_sequential(hl_host_windows *host, hl_host_handle file, void *buffer,
                                                 uint64_t size, int writing) {
    IO_STATUS_BLOCK status_block;
    NTSTATUS status;
    HANDLE object = NULL;
    if (!hl_windows_file_borrow(host, file, &object, NULL, NULL))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    status = writing ? host->nt.write_file(object, NULL, NULL, NULL, &status_block, buffer,
                                           hl_windows_transfer_size(size), NULL, NULL)
                     : host->nt.read_file(object, NULL, NULL, NULL, &status_block, buffer,
                                          hl_windows_transfer_size(size), NULL, NULL);
    if (status == HL_NT_END_OF_FILE) return hl_windows_result(HL_STATUS_OK, 0, 0);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    return hl_windows_result(HL_STATUS_OK, (uint64_t)status_block.Information, 0);
}

static hl_host_result hl_windows_file_read(void *context, hl_host_handle file, void *output, uint64_t output_size) {
    if (output_size != 0 && output == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_windows_file_sequential(context, file, output, output_size, 0);
}

static hl_host_result hl_windows_file_write(void *context, hl_host_handle file, const void *input,
                                            uint64_t input_size) {
    if (input_size != 0 && input == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_windows_file_sequential(context, file, (void *)(size_t)input, input_size, 1);
}

/*
 * One indivisible append. The offset is the NT "write to end of file" sentinel,
 * and the handle was opened with FILE_APPEND_DATA, which is what makes the
 * seek-to-end and the write a single filesystem operation. Two handles
 * appending concurrently were measured to concatenate cleanly.
 */
static hl_host_result hl_windows_append_bytes(hl_host_windows *host, hl_host_handle file, const void *data,
                                              uint64_t size) {
    IO_STATUS_BLOCK status_block;
    LARGE_INTEGER where;
    NTSTATUS status;
    HANDLE object = NULL;
    uint32_t access = 0;
    if (!hl_windows_file_borrow(host, file, &object, &access, NULL))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if ((access & HL_HOST_FILE_APPEND) == 0) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    where.HighPart = -1;
    where.LowPart = 0xFFFFFFFFu; /* FILE_WRITE_TO_END_OF_FILE */
    status = host->nt.write_file(object, NULL, NULL, NULL, &status_block, data, hl_windows_transfer_size(size), &where,
                                 NULL);
    if (status != HL_NT_SUCCESS) return hl_windows_nt_result(host, status);
    return hl_windows_result(HL_STATUS_OK, (uint64_t)status_block.Information, 0);
}

static hl_host_result hl_windows_file_append(void *context, hl_host_handle file, hl_host_const_bytes input) {
    if (input.size != 0 && input.data == NULL) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_windows_append_bytes(context, file, input.data, input.size);
}

/* --- vectored transfers ----------------------------------------------------- */

static hl_status hl_windows_vectors_valid(const hl_host_iovec *vectors, uint32_t count, uint64_t *out_total) {
    uint64_t total = 0;
    uint32_t index;
    if (count > (uint32_t)HL_HOST_FILE_IOV_MAX) return HL_STATUS_INVALID_ARGUMENT;
    if (count != 0 && vectors == NULL) return HL_STATUS_INVALID_ARGUMENT;
    for (index = 0; index < count; ++index) {
        if (vectors[index].size != 0 && vectors[index].address == 0) return HL_STATUS_INVALID_ARGUMENT;
        if (vectors[index].size > UINT64_MAX - total) return HL_STATUS_INVALID_ARGUMENT;
        total += vectors[index].size;
    }
    *out_total = total;
    return HL_STATUS_OK;
}

/*
 * Windows has no scatter/gather equivalent of readv(2) for ordinary files --
 * ReadFileScatter demands page-aligned, unbuffered, whole-page segments -- so
 * these iterate. The loop stops on the first short transfer, which is what a
 * caller must already tolerate from readv on a POSIX host.
 */
static hl_host_result hl_windows_file_vector(hl_host_windows *host, hl_host_handle file, const hl_host_iovec *vectors,
                                             uint32_t count, uint64_t offset, int positioned, int writing) {
    uint64_t total = 0;
    uint64_t moved = 0;
    uint32_t index;
    const hl_status valid = hl_windows_vectors_valid(vectors, count, &total);
    if (valid != HL_STATUS_OK) return hl_windows_result(valid, 0, 0);
    for (index = 0; index < count; ++index) {
        void *buffer = (void *)(size_t)vectors[index].address;
        hl_host_result step;
        if (vectors[index].size == 0) continue;
        step = positioned ? hl_windows_file_positioned(host, file, offset + moved, buffer, vectors[index].size, writing)
                          : hl_windows_file_sequential(host, file, buffer, vectors[index].size, writing);
        if (step.status != HL_STATUS_OK) return moved != 0 ? hl_windows_result(HL_STATUS_OK, moved, 0) : step;
        moved += step.value;
        if (step.value < vectors[index].size) break;
    }
    return hl_windows_result(HL_STATUS_OK, moved, 0);
}

static hl_host_result hl_windows_file_readv(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                            uint32_t count) {
    return hl_windows_file_vector(context, file, vectors, count, 0, 0, 0);
}

static hl_host_result hl_windows_file_writev(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                             uint32_t count) {
    return hl_windows_file_vector(context, file, vectors, count, 0, 0, 1);
}

static hl_host_result hl_windows_file_readv_at(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                               uint32_t count, uint64_t offset) {
    return hl_windows_file_vector(context, file, vectors, count, offset, 1, 0);
}

static hl_host_result hl_windows_file_writev_at(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                                uint32_t count, uint64_t offset) {
    return hl_windows_file_vector(context, file, vectors, count, offset, 1, 1);
}

/* An append must stay indivisible, so the vectors are gathered into one buffer
 * and written once rather than appended in sequence. */
static hl_host_result hl_windows_file_appendv(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                              uint32_t count) {
    hl_host_windows *host = context;
    uint64_t total = 0;
    uint64_t at = 0;
    uint32_t index;
    unsigned char *gathered;
    hl_host_result result;
    const hl_status valid = hl_windows_vectors_valid(vectors, count, &total);
    if (valid != HL_STATUS_OK) return hl_windows_result(valid, 0, 0);
    if (total == 0) return hl_windows_append_bytes(host, file, "", 0);
    if (total > HL_WINDOWS_IO_MAX) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    gathered = malloc((size_t)total);
    if (gathered == NULL) return hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    for (index = 0; index < count; ++index) {
        memcpy(gathered + at, (const void *)(size_t)vectors[index].address, (size_t)vectors[index].size);
        at += vectors[index].size;
    }
    result = hl_windows_append_bytes(host, file, gathered, total);
    free(gathered);
    return result;
}
