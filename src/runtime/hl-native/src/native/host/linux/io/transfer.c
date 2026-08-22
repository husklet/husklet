static hl_host_result hl_linux_transfer_channel_pair(void *context) {
    hl_host_linux *host = context;
    int pair[2];
    hl_host_result first;
    hl_host_result second;
    if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, pair) != 0) return hl_linux_errno_result();
    first = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_TRANSFER, pair[0], NULL, NULL, 0, -1);
    if (first.status != HL_STATUS_OK) {
        close(pair[0]);
        close(pair[1]);
        return first;
    }
    second = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_TRANSFER, pair[1], NULL, NULL, 0, -1);
    if (second.status != HL_STATUS_OK) {
        (void)hl_linux_close_descriptor(host, first.value);
        close(pair[1]);
        return second;
    }
    return hl_linux_result(HL_STATUS_OK, first.value, second.value);
}

static hl_host_result hl_linux_transfer_send(void *context, hl_host_handle channel, hl_host_const_bytes data,
                                             const hl_host_transfer_attachment *attachments, uint32_t count) {
    hl_host_linux *host = context;
    hl_linux_transfer_wire wire = {0};
    uint8_t control[CMSG_SPACE(sizeof(int) * HL_HOST_TRANSFER_MAX_ATTACHMENTS)] = {0};
    struct iovec vector = {&wire, sizeof(wire)};
    struct msghdr message = {0};
    int descriptors[HL_HOST_TRANSFER_MAX_ATTACHMENTS];
    int channel_fd;
    uint32_t index;
    if (data.size > HL_HOST_TRANSFER_MAX_DATA || (data.size != 0 && data.data == NULL) ||
        count > HL_HOST_TRANSFER_MAX_ATTACHMENTS || (count != 0 && attachments == NULL))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    channel_fd = hl_linux_descriptor(host, channel, HL_LINUX_HANDLE_TRANSFER, HL_LINUX_HANDLE_TRANSFER);
    for (index = 0; index < count && channel_fd >= 0; ++index) {
        hl_linux_handle_entry *entry = hl_linux_lookup_locked(host, attachments[index].object, HL_LINUX_HANDLE_COUNTER);
        uint32_t valid =
            HL_HOST_TRANSFER_READ | HL_HOST_TRANSFER_WRITE | HL_HOST_TRANSFER_WAIT | HL_HOST_TRANSFER_CONTROL;
        if (entry == NULL || attachments[index].kind != HL_HOST_TRANSFER_KIND_COUNTER ||
            (attachments[index].rights & ~valid) != 0 ||
            (attachments[index].rights & entry->reserved) != attachments[index].rights) {
            channel_fd = -1;
            break;
        }
        descriptors[index] = entry->descriptor;
        wire.flags[index] = (uint32_t)entry->size;
        wire.rights[index] = attachments[index].rights;
    }
    pthread_mutex_unlock(&host->lock);
    if (channel_fd < 0) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    wire.data_size = (uint32_t)data.size;
    wire.attachment_count = count;
    if (data.size != 0) memcpy(wire.data, data.data, data.size);
    message.msg_iov = &vector;
    message.msg_iovlen = 1;
    if (count != 0) {
        struct cmsghdr *header;
        message.msg_control = control;
        message.msg_controllen = CMSG_SPACE(sizeof(int) * count);
        header = CMSG_FIRSTHDR(&message);
        header->cmsg_level = SOL_SOCKET;
        header->cmsg_type = SCM_RIGHTS;
        header->cmsg_len = CMSG_LEN(sizeof(int) * count);
        memcpy(CMSG_DATA(header), descriptors, sizeof(int) * count);
    }
    return sendmsg(channel_fd, &message, 0) == (ssize_t)sizeof(wire) ? hl_linux_result(HL_STATUS_OK, 0, 0)
                                                                     : hl_linux_errno_result();
}

static hl_host_result hl_linux_transfer_receive(void *context, hl_host_handle channel, hl_host_bytes data,
                                                hl_host_transfer_attachment *attachments, uint32_t capacity) {
    hl_host_linux *host = context;
    hl_linux_transfer_wire wire;
    uint8_t control[CMSG_SPACE(sizeof(int) * HL_HOST_TRANSFER_MAX_ATTACHMENTS)] = {0};
    struct iovec vector = {&wire, sizeof(wire)};
    struct msghdr message = {0};
    int channel_fd;
    int received[HL_HOST_TRANSFER_MAX_ATTACHMENTS];
    uint32_t index;
    ssize_t bytes;
    pthread_mutex_lock(&host->lock);
    channel_fd = hl_linux_descriptor(host, channel, HL_LINUX_HANDLE_TRANSFER, HL_LINUX_HANDLE_TRANSFER);
    pthread_mutex_unlock(&host->lock);
    if (channel_fd < 0 || (data.size != 0 && data.data == NULL) || (capacity != 0 && attachments == NULL))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    bytes = recv(channel_fd, &wire, sizeof(wire), MSG_PEEK);
    if (bytes < 0) return hl_linux_errno_result();
    if (bytes != (ssize_t)sizeof(wire) || wire.data_size > HL_HOST_TRANSFER_MAX_DATA ||
        wire.attachment_count > HL_HOST_TRANSFER_MAX_ATTACHMENTS)
        return hl_linux_result(HL_STATUS_CORRUPT, 0, 0);
    if (wire.data_size > data.size || wire.attachment_count > capacity)
        return hl_linux_result(HL_STATUS_RESOURCE_LIMIT, wire.data_size, wire.attachment_count);
    message.msg_iov = &vector;
    message.msg_iovlen = 1;
    message.msg_control = control;
    message.msg_controllen = sizeof(control);
    bytes = recvmsg(channel_fd, &message, MSG_CMSG_CLOEXEC);
    if (bytes != (ssize_t)sizeof(wire))
        return bytes < 0 ? hl_linux_errno_result() : hl_linux_result(HL_STATUS_CORRUPT, 0, 0);
    if (wire.attachment_count != 0) {
        struct cmsghdr *header = CMSG_FIRSTHDR(&message);
        if (header == NULL || header->cmsg_level != SOL_SOCKET || header->cmsg_type != SCM_RIGHTS ||
            header->cmsg_len != CMSG_LEN(sizeof(int) * wire.attachment_count))
            return hl_linux_result(HL_STATUS_CORRUPT, 0, 0);
        memcpy(received, CMSG_DATA(header), sizeof(int) * wire.attachment_count);
    }
    for (index = 0; index < wire.attachment_count; ++index) {
        hl_host_result installed =
            hl_linux_allocate_handle(host, HL_LINUX_HANDLE_COUNTER, received[index], NULL, NULL, wire.flags[index], -1);
        if (installed.status != HL_STATUS_OK) {
            uint32_t rest;
            close(received[index]);
            for (rest = index + 1; rest < wire.attachment_count; ++rest)
                close(received[rest]);
            for (rest = 0; rest < index; ++rest)
                (void)hl_linux_close_descriptor(host, attachments[rest].object);
            return installed;
        }
        pthread_mutex_lock(&host->lock);
        hl_linux_lookup_locked(host, installed.value, HL_LINUX_HANDLE_COUNTER)->reserved = (uint16_t)wire.rights[index];
        pthread_mutex_unlock(&host->lock);
        attachments[index] =
            (hl_host_transfer_attachment){installed.value, HL_HOST_TRANSFER_KIND_COUNTER, wire.rights[index]};
    }
    if (wire.data_size != 0) memcpy(data.data, wire.data, wire.data_size);
    return hl_linux_result(HL_STATUS_OK, wire.data_size, wire.attachment_count);
}

static hl_host_result hl_linux_transfer_duplicate(void *context, hl_host_handle channel) {
    hl_host_linux *host = context;
    int descriptor;
    hl_host_result result;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, channel, HL_LINUX_HANDLE_TRANSFER, HL_LINUX_HANDLE_TRANSFER);
    descriptor = descriptor < 0 ? -1 : dup(descriptor);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) return hl_linux_errno_result();
    result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_TRANSFER, descriptor, NULL, NULL, 0, -1);
    if (result.status != HL_STATUS_OK) close(descriptor);
    return result;
}
