/* --- terminal ---------------------------------------------------------------------------
 *
 * The device, and only the device. Everything the guest's terminal vocabulary is made of -- the
 * attribute structure, its control characters, canonical buffering, line editing, signal
 * generation, minimum-and-timeout reads, output post-processing -- is above this and is not
 * reachable from here on purpose.
 *
 * The five mode bits are the abstract capabilities the contract names, mapped onto the five native
 * flags that carry the same meaning. Nothing else in the native attributes is read or written, so
 * a caller that sets a mode gets exactly the change it named and no adjacent policy it did not.
 */
static hl_host_result hl_macos_terminal_probe(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    int descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* Answered from the terminal line discipline itself rather than from the object type, which is
     * the distinction the contract exists for: a character device is not a terminal. */
    return hl_macos_result(HL_STATUS_OK, isatty(descriptor) != 0 ? 1u : 0u, 0);
}

static hl_host_result hl_macos_terminal_get_mode(void *context, hl_host_handle handle, uint32_t *mode) {
    hl_host_macos *host = context;
    struct termios attributes;
    int descriptor;
    uint32_t value = 0;
    if (mode == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (tcgetattr(descriptor, &attributes) != 0) return hl_macos_errno();
    if ((attributes.c_lflag & ICANON) == 0) value |= HL_HOST_TERMINAL_RAW_INPUT;
    if ((attributes.c_lflag & ECHO) != 0) value |= HL_HOST_TERMINAL_ECHO;
    if ((attributes.c_lflag & ISIG) != 0) value |= HL_HOST_TERMINAL_SIGNALS;
    if ((attributes.c_iflag & IXON) != 0) value |= HL_HOST_TERMINAL_FLOW_CONTROL;
    if ((attributes.c_oflag & OPOST) != 0) value |= HL_HOST_TERMINAL_OUTPUT_PROCESSING;
    *mode = value;
    return hl_macos_result(HL_STATUS_OK, value, 0);
}

static hl_host_result hl_macos_terminal_set_mode(void *context, hl_host_handle handle, uint32_t mode) {
    hl_host_macos *host = context;
    struct termios attributes;
    int descriptor;
    if ((mode & ~(uint32_t)(HL_HOST_TERMINAL_RAW_INPUT | HL_HOST_TERMINAL_ECHO | HL_HOST_TERMINAL_SIGNALS |
                            HL_HOST_TERMINAL_FLOW_CONTROL | HL_HOST_TERMINAL_OUTPUT_PROCESSING)) != 0)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (tcgetattr(descriptor, &attributes) != 0) return hl_macos_errno();
    if ((mode & HL_HOST_TERMINAL_RAW_INPUT) != 0)
        attributes.c_lflag &= (tcflag_t)~ICANON;
    else
        attributes.c_lflag |= (tcflag_t)ICANON;
    if ((mode & HL_HOST_TERMINAL_ECHO) != 0)
        attributes.c_lflag |= (tcflag_t)ECHO;
    else
        attributes.c_lflag &= (tcflag_t)~ECHO;
    if ((mode & HL_HOST_TERMINAL_SIGNALS) != 0)
        attributes.c_lflag |= (tcflag_t)ISIG;
    else
        attributes.c_lflag &= (tcflag_t)~ISIG;
    if ((mode & HL_HOST_TERMINAL_FLOW_CONTROL) != 0)
        attributes.c_iflag |= (tcflag_t)IXON;
    else
        attributes.c_iflag &= (tcflag_t)~IXON;
    if ((mode & HL_HOST_TERMINAL_OUTPUT_PROCESSING) != 0)
        attributes.c_oflag |= (tcflag_t)OPOST;
    else
        attributes.c_oflag &= (tcflag_t)~OPOST;
    /* Applied now rather than after the queued output drains: a caller turning echo off before
     * asking for a secret cannot be made to wait on a writer it does not control. */
    if (tcsetattr(descriptor, TCSANOW, &attributes) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, mode, 0);
}

static hl_host_result hl_macos_terminal_get_size(void *context, hl_host_handle handle, hl_host_terminal_size *size) {
    hl_host_macos *host = context;
    struct winsize window;
    int descriptor;
    if (size == NULL) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(&window, 0, sizeof(window));
    if (ioctl(descriptor, TIOCGWINSZ, &window) != 0) return hl_macos_errno();
    size->columns = window.ws_col;
    size->rows = window.ws_row;
    size->pixel_width = window.ws_xpixel;
    size->pixel_height = window.ws_ypixel;
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_terminal_set_size(void *context, hl_host_handle handle,
                                                 const hl_host_terminal_size *size) {
    hl_host_macos *host = context;
    struct winsize window;
    int descriptor;
    if (size == NULL || size->columns > UINT16_MAX || size->rows > UINT16_MAX || size->pixel_width > UINT16_MAX ||
        size->pixel_height > UINT16_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(&window, 0, sizeof(window));
    window.ws_col = (unsigned short)size->columns;
    window.ws_row = (unsigned short)size->rows;
    window.ws_xpixel = (unsigned short)size->pixel_width;
    window.ws_ypixel = (unsigned short)size->pixel_height;
    if (ioctl(descriptor, TIOCSWINSZ, &window) != 0) return hl_macos_errno();
    return hl_macos_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_macos_terminal_read(void *context, hl_host_handle handle, hl_host_bytes output) {
    hl_host_macos *host = context;
    int descriptor;
    ssize_t count;
    if ((output.size != 0 && output.data == NULL) || output.size > SSIZE_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = read(descriptor, output.data, output.size);
    return count >= 0 ? hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_macos_errno();
}

static hl_host_result hl_macos_terminal_write(void *context, hl_host_handle handle, hl_host_const_bytes input) {
    hl_host_macos *host = context;
    int descriptor;
    ssize_t count;
    if ((input.size != 0 && input.data == NULL) || input.size > SSIZE_MAX)
        return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    descriptor = hl_macos_file_descriptor(host, handle, 0);
    if (descriptor < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    count = write(descriptor, input.data, input.size);
    return count >= 0 ? hl_macos_result(HL_STATUS_OK, (uint64_t)count, 0) : hl_macos_errno();
}

/*
 * Typed absence, not an oversight. On this host a resize is delivered as a process-directed signal
 * that the engine's own signal machinery already owns end to end, so there is no separate object
 * for this to hand back -- and manufacturing one would mean installing a process-wide handler
 * underneath the layer that is already handling that signal, which is a worse bargain than saying
 * so. The operation exists in the contract for a host where the resize arrives in the input stream
 * instead, where waiting for input and then reading it is a deadlock rather than a composition.
 */
static hl_host_result hl_macos_terminal_size_change_event(void *context, hl_host_handle handle) {
    hl_host_macos *host = context;
    if (hl_macos_file_descriptor(host, handle, 0) < 0) return hl_macos_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    return hl_macos_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
}

