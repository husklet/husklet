#include "files.h"
#include "handles.h"

#include <errno.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <unistd.h>
#include <string.h>

enum { OP_OPEN = 1, OP_READ = 2, OP_WRITE = 3, OP_SEEK = 4, OP_STAT = 5, OP_POLL = 6, OP_CLOSE = 7, OP_IOCTL = 8 };

static const hl_host_file_services *underlying;
static hl_host_file_services composite;
static void *underlying_context;
static hl_provider_client *provider;
static hl_provider_handles handles;

typedef struct provider_readiness_state {
    _Atomic uint32_t ready;
    _Atomic uint32_t subscribed;
    uint64_t subscription;
    uint16_t generation;
    pid_t owner;

    struct {
        _Atomic uint32_t active;
        void (*notify)(void *, uint64_t);
        void *opaque;
        uint64_t token;
    } listeners[16];
} provider_readiness_state;

static provider_readiness_state readiness_states[HL_PROVIDER_HANDLE_MAX];

static void put32(unsigned char *bytes, uint32_t value) {
    bytes[0] = (unsigned char)value;
    bytes[1] = (unsigned char)(value >> 8);
    bytes[2] = (unsigned char)(value >> 16);
    bytes[3] = (unsigned char)(value >> 24);
}

static void put64(unsigned char *bytes, uint64_t value) {
    put32(bytes, (uint32_t)value);
    put32(bytes + 4, (uint32_t)(value >> 32));
}

static uint32_t get32(const unsigned char *bytes) {
    return (uint32_t)bytes[0] | (uint32_t)bytes[1] << 8 | (uint32_t)bytes[2] << 16 | (uint32_t)bytes[3] << 24;
}

static uint64_t get64(const unsigned char *bytes) {
    return (uint64_t)get32(bytes) | (uint64_t)get32(bytes + 4) << 32;
}

static hl_host_result failure(int error) {
    hl_status status = error == EBADF                      ? HL_STATUS_NOT_FOUND
                       : error == EACCES || error == EPERM ? HL_STATUS_PERMISSION_DENIED
                       : error == ETIMEDOUT                ? HL_STATUS_TIMED_OUT
                       : error == ENOMEM                   ? HL_STATUS_OUT_OF_MEMORY
                       : error == EMFILE                   ? HL_STATUS_RESOURCE_LIMIT
                       : error == ECONNRESET               ? HL_STATUS_DISCONNECTED
                                                           : HL_STATUS_IO;
    return (hl_host_result){.status = (int32_t)status, .detail = (uint64_t)(unsigned)error};
}

static hl_host_result request(const unsigned char *payload, uint32_t size, hl_provider_reply *reply) {
    int status = hl_provider_client_request(provider, payload, size, 5000, reply);
    if (status != 0) return failure(-status);
    if (reply->linux_errno != 0) return failure(reply->linux_errno);
    return (hl_host_result){.status = HL_STATUS_OK};
}

static int remote(hl_host_handle handle, uint64_t *out) {
    return hl_provider_handle_get(&handles, handle, out);
}

static provider_readiness_state *readiness_state(hl_host_handle handle) {
    uint32_t raw = (uint32_t)handle;
    if (!hl_provider_handle_is(handle) || raw == 0 || raw > HL_PROVIDER_HANDLE_MAX) return NULL;
    return &readiness_states[raw - 1];
}

static uint32_t readiness_decode(unsigned char states) {
    return (states & 1u ? 1u : 0u) | (states & 2u ? 2u : 0u) | (states & 8u ? 8u : 0u) | (states & 4u ? 16u : 0u);
}

static void provider_readiness_wake(void *opaque, uint64_t subscription) {
    provider_readiness_state *state = opaque;
    hl_provider_reply event;
    uint64_t lost = 0;
    int status;
    if (state == NULL || state->subscription != subscription || state->owner != getpid()) return;
    do {
        status = hl_provider_client_readiness(provider, subscription, &event, &lost);
        if (status == 0) {
            if (event.size == 2 && event.bytes[0] == OP_POLL)
                atomic_store(&state->ready, readiness_decode(event.bytes[1]));
            hl_provider_reply_destroy(&event);
            for (uint32_t i = 0; i < 16; ++i)
                if (atomic_load(&state->listeners[i].active) != 0)
                    state->listeners[i].notify(state->listeners[i].opaque, state->listeners[i].token);
        }
    } while (status == 0);
    if (lost != 0) atomic_fetch_or(&state->ready, 8u);
    if (status == -ECONNRESET || status == -EPIPE) atomic_store(&state->ready, 8u | 16u);
}

int hl_provider_files_subscribe(hl_host_handle handle, uint32_t interests, void (*notify)(void *, uint64_t),
                                void *opaque, uint64_t token) {
    provider_readiness_state *state = readiness_state(handle);
    if (state == NULL || notify == NULL) return -EINVAL;
    for (uint32_t i = 0; i < 16; ++i) {
        uint32_t inactive = 0;
        if (!atomic_compare_exchange_strong(&state->listeners[i].active, &inactive, 2)) continue;
        state->listeners[i].notify = notify;
        state->listeners[i].opaque = opaque;
        state->listeners[i].token = token;
        atomic_store(&state->listeners[i].active, 1);
        uint32_t ready = hl_provider_files_readiness(handle, interests);
        /* Registration itself is an edge when the service is already ready.
         * The remote subscription may predate this listener (for example a
         * poll() followed by epoll_ctl()), so no new wire event is guaranteed. */
        if (ready != 0) notify(opaque, token);
        return 0;
    }
    return -ENOSPC;
}

void hl_provider_files_unsubscribe(hl_host_handle handle, void *opaque, uint64_t token) {
    provider_readiness_state *state = readiness_state(handle);
    if (state == NULL) return;
    for (uint32_t i = 0; i < 16; ++i)
        if (atomic_load(&state->listeners[i].active) != 0 && state->listeners[i].opaque == opaque &&
            state->listeners[i].token == token)
            atomic_store(&state->listeners[i].active, 0);
}

hl_host_result hl_provider_files_open_service(uint64_t service, uint32_t access) {
    unsigned char payload[10] = {OP_OPEN};
    hl_provider_reply reply;
    hl_host_handle local;
    hl_host_result result;
    put64(payload + 1, service);
    payload[9] = (unsigned char)((access & HL_HOST_FILE_READ ? 1 : 0) | (access & HL_HOST_FILE_WRITE ? 2 : 0));
    result = request(payload, sizeof(payload), &reply);
    if (result.status == HL_STATUS_OK && (reply.size != 9 || reply.bytes[0] != OP_OPEN)) result = failure(EPROTO);
    if (result.status == HL_STATUS_OK && hl_provider_handle_open(&handles, get64(reply.bytes + 1), &local) != 0)
        result = failure(EMFILE);
    if (result.status == HL_STATUS_OK) {
        provider_readiness_state *state = readiness_state(local);
        if (state != NULL) {
            memset(state, 0, sizeof(*state));
            state->generation = (uint16_t)(local >> 32);
        }
    }
    if (result.status == HL_STATUS_OK) {
        result.value = local;
        result.detail = access;
    }
    hl_provider_reply_destroy(&reply);
    return result;
}

void hl_provider_files_ioctl_result_destroy(hl_provider_ioctl_result *result) {
    if (result == NULL) return;
    for (uint32_t i = 0; i < result->write_count; ++i) {
        free(result->writes[i].bytes);
        result->writes[i].bytes = NULL;
    }
    result->write_count = 0;
}

hl_host_result hl_provider_files_ioctl(hl_host_handle handle, uint64_t command, unsigned char *argument, uint32_t size,
                                       hl_provider_ioctl_result *output) {
    unsigned char *payload;
    hl_provider_reply reply;
    hl_host_result result;
    uint64_t id;
    uint32_t reply_size, write_count, offset, transferred;
    if (output == NULL || size > 16384 || (size != 0 && argument == NULL)) return failure(EINVAL);
    memset(output, 0, sizeof(*output));
    if (remote(handle, &id) != 0) return failure(EBADF);
    payload = malloc(21u + size);
    if (payload == NULL) return failure(ENOMEM);
    payload[0] = OP_IOCTL;
    put64(payload + 1, id);
    put64(payload + 9, command);
    put32(payload + 17, size);
    if (size != 0) memcpy(payload + 21, argument, size);
    result = request(payload, 21u + size, &reply);
    free(payload);
    if (result.status != HL_STATUS_OK) return result;
    if (reply.size < 13 || reply.bytes[0] != OP_IOCTL) {
        hl_provider_reply_destroy(&reply);
        return failure(EPROTO);
    }
    reply_size = get32(reply.bytes + 9);
    offset = 13u + reply_size;
    if (reply_size != size || reply.size < offset + 4u) {
        hl_provider_reply_destroy(&reply);
        return failure(EPROTO);
    }
    if (size != 0) memcpy(argument, reply.bytes + 13, size);
    write_count = get32(reply.bytes + offset);
    offset += 4;
    transferred = size;
    if (write_count > HL_PROVIDER_IOCTL_WRITE_MAX) {
        hl_provider_reply_destroy(&reply);
        return failure(EPROTO);
    }
    for (uint32_t i = 0; i < write_count; ++i) {
        uint32_t write_size;
        if (reply.size - offset < 12u) goto malformed;
        output->writes[i].address = get64(reply.bytes + offset);
        write_size = get32(reply.bytes + offset + 8);
        offset += 12;
        if (write_size > 16384u - transferred || reply.size - offset < write_size) goto malformed;
        output->writes[i].bytes = write_size == 0 ? NULL : malloc(write_size);
        if (write_size != 0 && output->writes[i].bytes == NULL) {
            output->write_count = i;
            hl_provider_files_ioctl_result_destroy(output);
            hl_provider_reply_destroy(&reply);
            return failure(ENOMEM);
        }
        if (write_size != 0) memcpy(output->writes[i].bytes, reply.bytes + offset, write_size);
        output->writes[i].size = write_size;
        output->write_count = i + 1;
        transferred += write_size;
        offset += write_size;
    }
    if (offset != reply.size) goto malformed;
    result.value = get64(reply.bytes + 1);
    hl_provider_reply_destroy(&reply);
    return result;

malformed:
    hl_provider_files_ioctl_result_destroy(output);
    hl_provider_reply_destroy(&reply);
    return failure(EPROTO);
}

static hl_host_result provider_read_at(void *context, hl_host_handle file, uint64_t offset, hl_host_bytes output) {
    unsigned char payload[21] = {OP_READ};
    hl_provider_reply reply;
    hl_host_result result;
    uint64_t id;
    uint32_t size = 0;
    if (!hl_provider_handle_is(file)) return underlying->read_at(underlying_context, file, offset, output);
    (void)context;
    if (output.size > UINT32_MAX || remote(file, &id) != 0) return failure(EINVAL);
    put64(payload + 1, id);
    put64(payload + 9, offset);
    put32(payload + 17, (uint32_t)output.size);
    result = request(payload, sizeof(payload), &reply);
    if (result.status == HL_STATUS_OK) {
        if (reply.size < 5 || reply.bytes[0] != OP_READ) {
            result = failure(EPROTO);
        } else {
            size = get32(reply.bytes + 1);
            if (size > output.size || reply.size != 5u + size) result = failure(EPROTO);
        }
    }
    if (result.status == HL_STATUS_OK) {
        memcpy(output.data, reply.bytes + 5, size);
        result.value = size;
    }
    hl_provider_reply_destroy(&reply);
    return result;
}

static hl_host_result provider_write_at(void *context, hl_host_handle file, uint64_t offset,
                                        hl_host_const_bytes input) {
    unsigned char payload[21 + 65536];
    hl_provider_reply reply;
    hl_host_result result;
    uint64_t id;
    if (!hl_provider_handle_is(file)) return underlying->write_at(underlying_context, file, offset, input);
    (void)context;
    if (input.size > 65536 || remote(file, &id) != 0) return failure(EINVAL);
    payload[0] = OP_WRITE;
    put64(payload + 1, id);
    put64(payload + 9, offset);
    put32(payload + 17, (uint32_t)input.size);
    memcpy(payload + 21, input.data, input.size);
    result = request(payload, (uint32_t)(21 + input.size), &reply);
    if (result.status == HL_STATUS_OK && (reply.size != 5 || reply.bytes[0] != OP_WRITE)) result = failure(EPROTO);
    if (result.status == HL_STATUS_OK) result.value = get32(reply.bytes + 1);
    hl_provider_reply_destroy(&reply);
    return result;
}

static hl_host_result provider_read(void *context, hl_host_handle file, void *output, uint64_t output_size) {
    if (!hl_provider_handle_is(file)) return underlying->read(underlying_context, file, output, output_size);
    return provider_read_at(context, file, UINT64_MAX, (hl_host_bytes){.data = output, .size = (size_t)output_size});
}

static hl_host_result provider_write(void *context, hl_host_handle file, const void *input, uint64_t input_size) {
    if (!hl_provider_handle_is(file)) return underlying->write(underlying_context, file, input, input_size);
    return provider_write_at(context, file, UINT64_MAX,
                             (hl_host_const_bytes){.data = input, .size = (size_t)input_size});
}

static hl_host_result provider_read_vectors(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                            uint32_t count, uint64_t offset, int positioned) {
    uint32_t index;
    uint64_t total = 0;
    if (!hl_provider_handle_is(file))
        return positioned ? underlying->readv_at(underlying_context, file, vectors, count, offset)
                          : underlying->readv(underlying_context, file, vectors, count);
    for (index = 0; index < count; ++index) {
        uint64_t at = positioned ? offset + total : UINT64_MAX;
        hl_host_result result = provider_read_at(
            context, file, at,
            (hl_host_bytes){.data = (void *)(uintptr_t)vectors[index].address, .size = (size_t)vectors[index].size});
        if (result.status != HL_STATUS_OK)
            return total != 0 ? (hl_host_result){.status = HL_STATUS_OK, .value = total} : result;
        total += result.value;
        if (result.value < vectors[index].size) break;
    }
    return (hl_host_result){.status = HL_STATUS_OK, .value = total};
}

static hl_host_result provider_write_vectors(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                             uint32_t count, uint64_t offset, int positioned) {
    uint32_t index;
    uint64_t total = 0;
    if (!hl_provider_handle_is(file))
        return positioned ? underlying->writev_at(underlying_context, file, vectors, count, offset)
                          : underlying->writev(underlying_context, file, vectors, count);
    for (index = 0; index < count; ++index) {
        uint64_t at = positioned ? offset + total : UINT64_MAX;
        hl_host_result result =
            provider_write_at(context, file, at,
                              (hl_host_const_bytes){.data = (const void *)(uintptr_t)vectors[index].address,
                                                    .size = (size_t)vectors[index].size});
        if (result.status != HL_STATUS_OK)
            return total != 0 ? (hl_host_result){.status = HL_STATUS_OK, .value = total} : result;
        total += result.value;
        if (result.value < vectors[index].size) break;
    }
    return (hl_host_result){.status = HL_STATUS_OK, .value = total};
}

static hl_host_result provider_readv(void *context, hl_host_handle file, const hl_host_iovec *vectors, uint32_t count) {
    return provider_read_vectors(context, file, vectors, count, 0, 0);
}

static hl_host_result provider_writev(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                      uint32_t count) {
    return provider_write_vectors(context, file, vectors, count, 0, 0);
}

static hl_host_result provider_readv_at(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                        uint32_t count, uint64_t offset) {
    return provider_read_vectors(context, file, vectors, count, offset, 1);
}

static hl_host_result provider_writev_at(void *context, hl_host_handle file, const hl_host_iovec *vectors,
                                         uint32_t count, uint64_t offset) {
    return provider_write_vectors(context, file, vectors, count, offset, 1);
}

static hl_host_result provider_metadata(void *context, hl_host_handle file, hl_host_file_metadata *output) {
    unsigned char payload[9] = {OP_STAT};
    hl_provider_reply reply;
    hl_host_result result;
    uint64_t id;
    if (!hl_provider_handle_is(file)) return underlying->metadata(underlying_context, file, output);
    (void)context;
    if (output == NULL || remote(file, &id) != 0) return failure(EINVAL);
    put64(payload + 1, id);
    result = request(payload, sizeof(payload), &reply);
    if (result.status == HL_STATUS_OK && (reply.size != 21 || reply.bytes[0] != OP_STAT)) result = failure(EPROTO);
    if (result.status == HL_STATUS_OK) {
        memset(output, 0, sizeof(*output));
        output->type = HL_HOST_FILE_TYPE_REGULAR;
        output->permissions = get32(reply.bytes + 1);
        output->user = get32(reply.bytes + 5);
        output->group = get32(reply.bytes + 9);
        output->size = get64(reply.bytes + 13);
        output->stable_object = id;
        output->link_count = 1;
    }
    hl_provider_reply_destroy(&reply);
    return result;
}

static hl_host_result provider_seek(void *context, hl_host_handle file, int64_t offset, uint32_t whence) {
    unsigned char payload[18] = {OP_SEEK};
    hl_provider_reply reply;
    hl_host_result result;
    uint64_t id;
    if (!hl_provider_handle_is(file)) return underlying->seek(underlying_context, file, offset, whence);
    (void)context;
    if (whence > 2 || remote(file, &id) != 0) return failure(EINVAL);
    put64(payload + 1, id);
    put64(payload + 9, (uint64_t)offset);
    payload[17] = (unsigned char)whence;
    result = request(payload, sizeof(payload), &reply);
    if (result.status == HL_STATUS_OK && (reply.size != 9 || reply.bytes[0] != OP_SEEK)) result = failure(EPROTO);
    if (result.status == HL_STATUS_OK) result.value = get64(reply.bytes + 1);
    hl_provider_reply_destroy(&reply);
    return result;
}

static hl_host_result provider_clone(void *context, hl_host_handle file) {
    if (!hl_provider_handle_is(file)) return underlying->clone_for_fork(underlying_context, file);
    (void)context;
    return hl_provider_handle_retain(&handles, file) == 0 ? (hl_host_result){.status = HL_STATUS_OK, .value = file}
                                                          : failure(EBADF);
}

static hl_host_result provider_close(void *context, hl_host_handle file) {
    unsigned char payload[9] = {OP_CLOSE};
    hl_provider_reply reply;
    hl_host_result result;
    uint64_t id = 0;
    int final;
    if (!hl_provider_handle_is(file)) return underlying->close(underlying_context, file);
    provider_readiness_state *state = readiness_state(file);
    if (state != NULL && state->owner == getpid() && atomic_load(&state->subscribed) != 0) {
        (void)hl_provider_client_unsubscribe(provider, state->subscription);
        atomic_store(&state->subscribed, 0);
    }
    (void)context;
    final = hl_provider_handle_release(&handles, file, &id);
    if (final < 0) return failure(EBADF);
    if (final == 0) return (hl_host_result){.status = HL_STATUS_OK};
    put64(payload + 1, id);
    result = request(payload, sizeof(payload), &reply);
    if (result.status == HL_STATUS_OK && (reply.size != 1 || reply.bytes[0] != OP_CLOSE)) result = failure(EPROTO);
    hl_provider_reply_destroy(&reply);
    return result;
}

int hl_provider_files_install(hl_host_services *services, hl_provider_client *client) {
    if (services == NULL || services->file == NULL || client == NULL || provider != NULL) return -EINVAL;
    underlying = services->file;
    underlying_context = services->context;
    provider = client;
    composite = *underlying;
    composite.read_at = provider_read_at;
    composite.write_at = provider_write_at;
    composite.metadata = provider_metadata;
    composite.read = provider_read;
    composite.write = provider_write;
    composite.readv = provider_readv;
    composite.writev = provider_writev;
    composite.readv_at = provider_readv_at;
    composite.writev_at = provider_writev_at;
    composite.close = provider_close;
    composite.clone_for_fork = provider_clone;
    composite.seek = provider_seek;
    services->file = &composite;
    return 0;
}

void hl_provider_files_revoke(void) {
    hl_provider_handles_revoke(&handles);
    memset(readiness_states, 0, sizeof(readiness_states));
    provider = NULL;
    underlying = NULL;
    underlying_context = NULL;
    memset(&composite, 0, sizeof(composite));
}

int hl_provider_files_is_handle(hl_host_handle handle) {
    return hl_provider_handle_is(handle);
}

uint32_t hl_provider_files_readiness(hl_host_handle handle, uint32_t interests) {
    unsigned char payload[10] = {OP_POLL};
    uint64_t id;
    provider_readiness_state *state = readiness_state(handle);
    if (remote(handle, &id) != 0) return 0;
    if (state == NULL) return 0;
    if (state->owner != getpid() || state->generation != (uint16_t)(handle >> 32)) {
        state->owner = getpid();
        state->generation = (uint16_t)(handle >> 32);
        state->subscription = ((uint64_t)(uint32_t)getpid() << 32) ^ handle ^ UINT64_C(0x5259000000000000);
        if (state->subscription == 0) state->subscription = 1;
        atomic_store(&state->ready, 0);
        atomic_store(&state->subscribed, 0);
    }
    put64(payload + 1, id);
    payload[9] = (unsigned char)(interests & 7u);
    if (atomic_load(&state->subscribed) == 0) {
        if (hl_provider_client_subscribe(provider, state->subscription, payload, sizeof(payload),
                                         provider_readiness_wake, state) != 0)
            return 8u | 16u;
        atomic_store(&state->subscribed, 1);
    }
    return atomic_load(&state->ready) & (interests | 8u | 16u);
}

uint32_t hl_provider_files_cached_readiness(hl_host_handle handle, uint32_t interests) {
    provider_readiness_state *state = readiness_state(handle);
    if (state == NULL || state->owner != getpid()) return 0;
    return atomic_load(&state->ready) & (interests | 8u | 16u);
}
