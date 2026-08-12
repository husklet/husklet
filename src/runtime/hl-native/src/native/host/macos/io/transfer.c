static hl_macos_transfer *hl_macos_transfer_lookup(hl_host_macos *host, hl_host_handle handle) {
    uint32_t index;
    if (!hl_macos_handle_index(handle, HL_MACOS_HANDLE_TRANSFER, host->transfer_capacity, &index) ||
        !host->transfers[index].active || host->transfers[index].generation != (uint32_t)(handle >> 32))
        return NULL;
    return &host->transfers[index];
}

static hl_host_result hl_macos_transfer_register(hl_host_macos *host, int descriptor) {
    uint32_t index;
    int adopted = hl_host_process_fd_private_adopt(descriptor);
    if (adopted < 0) return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    descriptor = adopted;
    pthread_mutex_lock(&host->lock);
    for (index = 0; index < host->transfer_capacity; ++index) {
        hl_macos_transfer *transfer = &host->transfers[index];
        if (transfer->active) continue;
        transfer->generation++;
        if (transfer->generation == 0) transfer->generation = 1;
        transfer->active = 1;
        transfer->descriptor = descriptor;
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_OK, hl_macos_handle(HL_MACOS_HANDLE_TRANSFER, index, transfer->generation), 0);
    }
    uint32_t capacity =
        hl_macos_grow_capacity(host->transfer_capacity, HL_MACOS_TRANSFER_CAPACITY, sizeof(*host->transfers));
    if (capacity == 0) {
        pthread_mutex_unlock(&host->lock);
        hl_host_process_fd_private_remove(descriptor);
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
    }
    hl_macos_transfer *grown = realloc(host->transfers, (size_t)capacity * sizeof(*grown));
    if (grown != NULL) {
        memset(grown + host->transfer_capacity, 0, (size_t)(capacity - host->transfer_capacity) * sizeof(*grown));
        index = host->transfer_capacity;
        host->transfers = grown;
        host->transfer_capacity = capacity;
        grown[index].generation = 1;
        grown[index].active = 1;
        grown[index].descriptor = descriptor;
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_OK, hl_macos_handle(HL_MACOS_HANDLE_TRANSFER, index, 1), 0);
    }
    pthread_mutex_unlock(&host->lock);
    hl_host_process_fd_private_remove(descriptor);
    return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, 0, 0);
}

typedef struct hl_macos_transfer_wire {
    uint32_t data_size;
    uint32_t attachment_count;
    uint32_t flags[HL_HOST_TRANSFER_MAX_ATTACHMENTS];
    uint32_t rights[HL_HOST_TRANSFER_MAX_ATTACHMENTS];
    uint8_t data[HL_HOST_TRANSFER_MAX_DATA];
} hl_macos_transfer_wire;

static hl_host_result hl_macos_counter_register(hl_host_macos *host, hl_macos_counter_object *object, uint32_t rights);
static hl_host_result hl_macos_transfer_close(void *context, hl_host_handle handle);

static hl_host_result hl_macos_transfer_channel_pair(void *context) {
    hl_host_macos *host = context;
    int pair[2];
    hl_host_result first;
    hl_host_result second;
    /* Darwin does not provide AF_UNIX SOCK_SEQPACKET; datagrams retain message boundaries. */
    if (socketpair(AF_UNIX, SOCK_DGRAM, 0, pair) != 0) return hl_macos_errno();
    (void)fcntl(pair[0], F_SETFD, FD_CLOEXEC);
    (void)fcntl(pair[1], F_SETFD, FD_CLOEXEC);
    first = hl_macos_transfer_register(host, pair[0]);
    if (first.status != HL_STATUS_OK) {
        close(pair[0]);
        close(pair[1]);
        return first;
    }
    second = hl_macos_transfer_register(host, pair[1]);
    if (second.status != HL_STATUS_OK) {
        close(pair[1]);
        (void)hl_macos_transfer_close(host, first.value);
        return second;
    }
    return hl_macos_result(HL_STATUS_OK, first.value, second.value);
}

static hl_host_result hl_macos_transfer_send(void *context, hl_host_handle channel, hl_host_const_bytes data,
                                             const hl_host_transfer_attachment *attachments, uint32_t count) {
    hl_host_macos *host = context;
    hl_macos_transfer_wire wire = {0};
    uint8_t control[CMSG_SPACE(sizeof(int) * HL_HOST_TRANSFER_MAX_ATTACHMENTS * 3u)] = {0};
    struct iovec vector = {&wire, sizeof(wire)};
    struct msghdr message = {0};
    int descriptors[HL_HOST_TRANSFER_MAX_ATTACHMENTS * 3u];
    int channel_fd = -1;
    uint32_t index;
    if (data.size > HL_HOST_TRANSFER_MAX_DATA || (data.size != 0 && data.data == NULL) ||
        count > HL_HOST_TRANSFER_MAX_ATTACHMENTS || (count != 0 && attachments == NULL))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    {
        hl_macos_transfer *transfer = hl_macos_transfer_lookup(host, channel);
        if (transfer != NULL) channel_fd = transfer->descriptor;
    }
    for (index = 0; index < count && channel_fd >= 0; ++index) {
        hl_macos_counter *counter = hl_macos_counter_lookup(host, attachments[index].object);
        uint32_t valid =
            HL_HOST_TRANSFER_READ | HL_HOST_TRANSFER_WRITE | HL_HOST_TRANSFER_WAIT | HL_HOST_TRANSFER_CONTROL;
        if (counter == NULL || attachments[index].kind != HL_HOST_TRANSFER_KIND_COUNTER ||
            (attachments[index].rights & ~valid) != 0 ||
            (attachments[index].rights & counter->rights) != attachments[index].rights) {
            channel_fd = -1;
            break;
        }
        descriptors[index * 3u] = counter->object->backing;
        descriptors[index * 3u + 1u] = counter->object->readable;
        descriptors[index * 3u + 2u] = counter->object->signal;
        wire.flags[index] = counter->object->shared->flags;
        wire.rights[index] = attachments[index].rights;
    }
    pthread_mutex_unlock(&host->lock);
    if (channel_fd < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    wire.data_size = (uint32_t)data.size;
    wire.attachment_count = count;
    if (data.size != 0) memcpy(wire.data, data.data, data.size);
    message.msg_iov = &vector;
    message.msg_iovlen = 1;
    if (count != 0) {
        struct cmsghdr *header;
        size_t descriptor_bytes = sizeof(int) * count * 3u;
        message.msg_control = control;
        message.msg_controllen = (socklen_t)CMSG_SPACE(descriptor_bytes);
        header = CMSG_FIRSTHDR(&message);
        header->cmsg_level = SOL_SOCKET;
        header->cmsg_type = SCM_RIGHTS;
        header->cmsg_len = (socklen_t)CMSG_LEN(descriptor_bytes);
        memcpy(CMSG_DATA(header), descriptors, descriptor_bytes);
    }
    return sendmsg(channel_fd, &message, 0) == (ssize_t)sizeof(wire) ? hl_macos_result(HL_STATUS_OK, 0, 0)
                                                                     : hl_macos_errno();
}

static hl_host_result hl_macos_transfer_receive(void *context, hl_host_handle channel, hl_host_bytes data,
                                                hl_host_transfer_attachment *attachments, uint32_t capacity) {
    hl_host_macos *host = context;
    hl_macos_transfer_wire wire;
    uint8_t control[CMSG_SPACE(sizeof(int) * HL_HOST_TRANSFER_MAX_ATTACHMENTS * 3u)] = {0};
    struct iovec vector = {&wire, sizeof(wire)};
    struct msghdr message = {0};
    int received[HL_HOST_TRANSFER_MAX_ATTACHMENTS * 3u];
    int channel_fd = -1;
    uint32_t index;
    ssize_t bytes;
    pthread_mutex_lock(&host->lock);
    {
        hl_macos_transfer *transfer = hl_macos_transfer_lookup(host, channel);
        if (transfer != NULL) channel_fd = transfer->descriptor;
    }
    pthread_mutex_unlock(&host->lock);
    if (channel_fd < 0 || (data.size != 0 && data.data == NULL) || (capacity != 0 && attachments == NULL))
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    bytes = recv(channel_fd, &wire, sizeof(wire), MSG_PEEK);
    if (bytes < 0) return hl_macos_errno();
    if (bytes != (ssize_t)sizeof(wire) || wire.data_size > HL_HOST_TRANSFER_MAX_DATA ||
        wire.attachment_count > HL_HOST_TRANSFER_MAX_ATTACHMENTS)
        return hl_macos_result(HL_STATUS_CORRUPT, 0, 0);
    if (wire.data_size > data.size || wire.attachment_count > capacity)
        return hl_macos_result(HL_STATUS_RESOURCE_LIMIT, wire.data_size, wire.attachment_count);
    message.msg_iov = &vector;
    message.msg_iovlen = 1;
    message.msg_control = control;
    message.msg_controllen = sizeof(control);
    bytes = recvmsg(channel_fd, &message, 0);
    if (bytes != (ssize_t)sizeof(wire)) return bytes < 0 ? hl_macos_errno() : hl_macos_result(HL_STATUS_CORRUPT, 0, 0);
    if (wire.attachment_count != 0) {
        struct cmsghdr *header = CMSG_FIRSTHDR(&message);
        size_t descriptor_bytes = sizeof(int) * wire.attachment_count * 3u;
        if (header == NULL || header->cmsg_level != SOL_SOCKET || header->cmsg_type != SCM_RIGHTS ||
            header->cmsg_len != CMSG_LEN(descriptor_bytes))
            return hl_macos_result(HL_STATUS_CORRUPT, 0, 0);
        memcpy(received, CMSG_DATA(header), descriptor_bytes);
    }
    for (index = 0; index < wire.attachment_count; ++index) {
        hl_macos_counter_object *object = calloc(1, sizeof(*object));
        hl_host_result installed;
        if (object == NULL) return hl_macos_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
        object->backing = received[index * 3u];
        object->readable = received[index * 3u + 1u];
        object->signal = received[index * 3u + 2u];
        object->shared = mmap(NULL, sizeof(*object->shared), PROT_READ | PROT_WRITE, MAP_SHARED, object->backing, 0);
        if (object->shared == MAP_FAILED) {
            close(object->backing);
            close(object->readable);
            close(object->signal);
            free(object);
            return hl_macos_errno();
        }
        installed = hl_macos_counter_register(host, object, wire.rights[index]);
        if (installed.status != HL_STATUS_OK) {
            munmap(object->shared, sizeof(*object->shared));
            close(object->backing);
            close(object->readable);
            close(object->signal);
            free(object);
            return installed;
        }
        attachments[index] =
            (hl_host_transfer_attachment){installed.value, HL_HOST_TRANSFER_KIND_COUNTER, wire.rights[index]};
    }
    if (wire.data_size != 0) memcpy(data.data, wire.data, wire.data_size);
    return hl_macos_result(HL_STATUS_OK, wire.data_size, wire.attachment_count);
}

static hl_host_result hl_macos_transfer_close(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    hl_macos_transfer *transfer;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    transfer = hl_macos_transfer_lookup(host, handle);
    if (transfer == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    descriptor = transfer->descriptor;
    transfer->active = 0;
    transfer->descriptor = -1;
    pthread_mutex_unlock(&host->lock);
    hl_host_process_fd_private_remove(descriptor);
    return close(descriptor) == 0 ? hl_macos_result(HL_STATUS_OK, 0, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_transfer_duplicate(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    hl_macos_transfer *transfer;
    int descriptor;
    pthread_mutex_lock(&host->lock);
    transfer = hl_macos_transfer_lookup(host, handle);
    descriptor = transfer == NULL ? -1 : dup(transfer->descriptor);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_macos_errno();
    {
        hl_host_result result = hl_macos_transfer_register(host, descriptor);
        if (result.status != HL_STATUS_OK) close(descriptor);
        return result;
    }
}
