#define _POSIX_C_SOURCE 200809L
#include "tree_files.h"

#include <errno.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>

enum {
    TREE_OPEN = 16,
    TREE_READ = 17,
    TREE_STAT = 18,
    TREE_LINK = 19,
    TREE_DENTS = 20,
    TREE_CLOSE = 21,
    TREE_WRITE = 22,
    TREE_APPEND = 23,
    TREE_TRUNCATE = 24,
    TREE_ERROR = 0xff,
    TREE_PAYLOAD = 4096,
    TREE_DATA = TREE_PAYLOAD - 5,
    TREE_WRITE_DATA = TREE_PAYLOAD - 21,
    TREE_APPEND_DATA = TREE_PAYLOAD - 13,
    TREE_HANDLE_MAX = 1024
};

#define TREE_HANDLE_TAG UINT64_C(0x4854000000000000)
#define TREE_HANDLE_MASK UINT64_C(0xffff000000000000)

typedef struct tree_slot {
    uint64_t remote;
    uint64_t offset;
    uint32_t references;
    uint32_t access;
    uint16_t generation;
    uint8_t kind;
    uint8_t live;
} tree_slot;

static const hl_host_file_services *underlying;
static hl_host_file_services composite;
static void *underlying_context;
static hl_provider_client *provider;
static tree_slot slots[TREE_HANDLE_MAX];
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;

static void put16(unsigned char *bytes, uint16_t value) {
    bytes[0] = (unsigned char)value;
    bytes[1] = (unsigned char)(value >> 8);
}

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

static uint16_t get16(const unsigned char *bytes) {
    return (uint16_t)((uint16_t)bytes[0] | (uint16_t)((uint16_t)bytes[1] << 8));
}

static uint32_t get32(const unsigned char *bytes) {
    return (uint32_t)bytes[0] | (uint32_t)bytes[1] << 8 | (uint32_t)bytes[2] << 16 | (uint32_t)bytes[3] << 24;
}

static uint64_t get64(const unsigned char *bytes) {
    return (uint64_t)get32(bytes) | (uint64_t)get32(bytes + 4) << 32;
}

static hl_host_result failure(int error) {
    hl_status status = error == ENOENT || error == EBADF       ? HL_STATUS_NOT_FOUND
                       : error == EACCES || error == EPERM     ? HL_STATUS_PERMISSION_DENIED
                       : error == EEXIST                       ? HL_STATUS_ALREADY_EXISTS
                       : error == ENOTDIR                      ? HL_STATUS_NOT_DIRECTORY
                       : error == EISDIR                       ? HL_STATUS_IS_DIRECTORY
                       : error == ELOOP                        ? HL_STATUS_SYMLINK_LOOP
                       : error == ENAMETOOLONG                 ? HL_STATUS_NAME_TOO_LONG
                       : error == EROFS                        ? HL_STATUS_READ_ONLY
                       : error == ENOMEM                       ? HL_STATUS_OUT_OF_MEMORY
                       : error == EMFILE || error == ENFILE    ? HL_STATUS_RESOURCE_LIMIT
                       : error == EINVAL                       ? HL_STATUS_INVALID_ARGUMENT
                       : error == ECONNRESET || error == EPIPE ? HL_STATUS_DISCONNECTED
                                                               : HL_STATUS_IO;
    return (hl_host_result){.status = (int32_t)status, .detail = (uint64_t)(unsigned)error};
}

static hl_host_result request(const unsigned char *payload, uint32_t size, hl_provider_reply *reply) {
    int status = hl_provider_client_request(provider, payload, size, 5000, reply);
    if (status != 0) return failure(-status);
    if (reply->size == 7 && reply->bytes[0] == TREE_ERROR && reply->bytes[5] == 0 && reply->bytes[6] == 0) {
        int error = (int)get32(reply->bytes + 1);
        if (error > 0 && error <= 4095) return failure(error);
    }
    return (hl_host_result){.status = HL_STATUS_OK};
}

static int decode(hl_host_handle handle, uint32_t *index, uint16_t *generation) {
    uint64_t raw;
    if ((handle & TREE_HANDLE_MASK) != TREE_HANDLE_TAG) return -EBADF;
    raw = handle & UINT64_C(0xffffffff);
    if (raw == 0 || raw > TREE_HANDLE_MAX) return -EBADF;
    *index = (uint32_t)(raw - 1);
    *generation = (uint16_t)(handle >> 32);
    return *generation == 0 ? -EBADF : 0;
}

static tree_slot *lookup(hl_host_handle handle) {
    uint32_t index;
    uint16_t generation;
    if (decode(handle, &index, &generation) != 0 || !slots[index].live || slots[index].generation != generation)
        return NULL;
    return &slots[index];
}

static int is_tree(hl_host_handle handle) {
    uint32_t index;
    uint16_t generation;
    return decode(handle, &index, &generation) == 0;
}

static int allocate(uint64_t remote, uint32_t access, uint8_t kind, hl_host_handle *output) {
    int status = -EMFILE;
    pthread_mutex_lock(&lock);
    for (uint32_t index = 0; index < TREE_HANDLE_MAX; ++index) {
        tree_slot *slot = &slots[index];
        if (slot->live) continue;
        slot->generation++;
        if (slot->generation == 0) slot->generation = 1;
        slot->remote = remote;
        slot->offset = 0;
        slot->references = 1;
        slot->access = access;
        slot->kind = kind;
        slot->live = 1;
        *output = TREE_HANDLE_TAG | (uint64_t)slot->generation << 32 | (uint64_t)(index + 1);
        status = 0;
        break;
    }
    pthread_mutex_unlock(&lock);
    return status;
}

static hl_host_result open_tree(uint64_t base, const char *path, size_t path_size, uint32_t access, uint32_t creation,
                                uint32_t permissions, uint32_t kind) {
    unsigned char payload[TREE_PAYLOAD];
    hl_provider_reply reply;
    hl_host_result result;
    hl_host_handle local;
    uint8_t flags = 0;
    if (provider == NULL || path == NULL || path_size == 0 || path_size > TREE_PAYLOAD - 18 || path_size > UINT16_MAX ||
        kind > HL_PROVIDER_TREE_LINK || (base == 0) != (path[0] == '/'))
        return failure(EINVAL);
    if ((access & HL_HOST_FILE_READ) != 0 || (access & HL_HOST_FILE_PATH_ONLY) != 0) flags |= 1;
    if ((access & HL_HOST_FILE_WRITE) != 0) flags |= 2;
    if ((creation & HL_HOST_FILE_CREATE) != 0) flags |= 4;
    if ((creation & HL_HOST_FILE_TRUNCATE) != 0) flags |= 8;
    if ((access & HL_HOST_FILE_APPEND) != 0) flags |= 16;
    if ((creation & HL_HOST_FILE_EXCLUSIVE) != 0) flags |= 32;
    if ((flags & 3u) == 0) flags |= 1;
    memset(payload, 0, 18);
    payload[0] = TREE_OPEN;
    payload[1] = (unsigned char)kind;
    put64(payload + 2, base);
    payload[10] = flags;
    put32(payload + 11, permissions & 07777u);
    put16(payload + 15, (uint16_t)path_size);
    memcpy(payload + 18, path, path_size);
    result = request(payload, (uint32_t)(18 + path_size), &reply);
    if (result.status == HL_STATUS_OK && (reply.size != 9 || reply.bytes[0] != TREE_OPEN)) result = failure(EPROTO);
    if (result.status == HL_STATUS_OK && allocate(get64(reply.bytes + 1), access, (uint8_t)kind, &local) != 0) {
        unsigned char close_payload[9] = {TREE_CLOSE};
        hl_provider_reply close_reply;
        put64(close_payload + 1, get64(reply.bytes + 1));
        if (request(close_payload, sizeof(close_payload), &close_reply).status == HL_STATUS_OK)
            hl_provider_reply_destroy(&close_reply);
        result = failure(EMFILE);
    }
    if (result.status == HL_STATUS_OK) {
        result.value = local;
        result.detail = access;
    }
    hl_provider_reply_destroy(&reply);
    return result;
}

hl_host_result hl_provider_tree_open_root(const char *path, size_t path_size, uint32_t access, uint32_t creation,
                                          uint32_t permissions, uint32_t kind) {
    return open_tree(0, path, path_size, access, creation, permissions, kind);
}

static hl_host_result tree_close(void *context, hl_host_handle file);
static hl_host_result tree_readlink(void *context, hl_host_handle file, hl_host_bytes output);

static hl_host_result tree_open_relative(void *context, hl_host_handle directory, const char *path, size_t path_size,
                                         uint32_t access, uint32_t creation, uint32_t permissions) {
    uint64_t remote;
    uint32_t kind = (access & HL_HOST_FILE_DIRECTORY) != 0  ? HL_PROVIDER_TREE_DIRECTORY
                    : (access & HL_HOST_FILE_NOFOLLOW) != 0 ? HL_PROVIDER_TREE_LINK
                                                            : HL_PROVIDER_TREE_FILE;
    if (!is_tree(directory))
        return underlying->open_relative(underlying_context, directory, path, path_size, access, creation, permissions);
    (void)context;
    pthread_mutex_lock(&lock);
    tree_slot *slot = lookup(directory);
    remote = slot == NULL ? 0 : slot->remote;
    pthread_mutex_unlock(&lock);
    if (remote == 0) return failure(EBADF);
    if ((access & (HL_HOST_FILE_NOFOLLOW | HL_HOST_FILE_PATH_ONLY)) == HL_HOST_FILE_NOFOLLOW) {
        unsigned char target;
        hl_host_result probe =
            open_tree(remote, path, path_size, HL_HOST_FILE_READ | HL_HOST_FILE_PATH_ONLY, 0, 0, HL_PROVIDER_TREE_LINK);
        hl_host_result inspected;
        if (probe.status != HL_STATUS_OK) return probe;
        inspected = tree_readlink(context, probe.value, (hl_host_bytes){.data = &target, .size = sizeof(target)});
        (void)tree_close(context, probe.value);
        if (inspected.status == HL_STATUS_OK) return failure(ELOOP);
        if (inspected.status != HL_STATUS_INVALID_ARGUMENT) return inspected;
    }
    return open_tree(remote, path, path_size, access, creation, permissions, kind);
}

static hl_host_result read_at_remote(uint64_t remote, uint64_t offset, hl_host_bytes output) {
    unsigned char payload[21] = {TREE_READ};
    hl_provider_reply reply;
    hl_host_result result;
    size_t total = 0;
    while (total < output.size) {
        uint32_t amount = (uint32_t)((output.size - total) > TREE_DATA ? TREE_DATA : output.size - total);
        put64(payload + 1, remote);
        put64(payload + 9, offset + total);
        put32(payload + 17, amount);
        result = request(payload, sizeof(payload), &reply);
        if (result.status != HL_STATUS_OK)
            return total == 0 ? result : (hl_host_result){.status = HL_STATUS_OK, .value = total};
        if (reply.size < 5 || reply.bytes[0] != TREE_READ || get32(reply.bytes + 1) > amount ||
            reply.size != 5u + get32(reply.bytes + 1)) {
            hl_provider_reply_destroy(&reply);
            return failure(EPROTO);
        }
        uint32_t count = get32(reply.bytes + 1);
        memcpy((unsigned char *)output.data + total, reply.bytes + 5, count);
        hl_provider_reply_destroy(&reply);
        total += count;
        if (count < amount) break;
    }
    return (hl_host_result){.status = HL_STATUS_OK, .value = total};
}

static hl_host_result tree_read_at(void *context, hl_host_handle file, uint64_t offset, hl_host_bytes output) {
    uint64_t remote;
    if (!is_tree(file)) return underlying->read_at(underlying_context, file, offset, output);
    (void)context;
    pthread_mutex_lock(&lock);
    tree_slot *slot = lookup(file);
    remote = slot == NULL ? 0 : slot->remote;
    pthread_mutex_unlock(&lock);
    return remote == 0 ? failure(EBADF) : read_at_remote(remote, offset, output);
}

static hl_host_result write_at_remote(uint64_t remote, uint64_t offset, hl_host_const_bytes input) {
    unsigned char payload[TREE_PAYLOAD];
    size_t total = 0;
    while (total < input.size) {
        hl_provider_reply reply;
        hl_host_result result;
        uint32_t amount = (uint32_t)((input.size - total) > TREE_WRITE_DATA ? TREE_WRITE_DATA : input.size - total);
        payload[0] = TREE_WRITE;
        put64(payload + 1, remote);
        put64(payload + 9, offset + total);
        put32(payload + 17, amount);
        memcpy(payload + 21, (const unsigned char *)input.data + total, amount);
        result = request(payload, 21u + amount, &reply);
        if (result.status != HL_STATUS_OK)
            return total == 0 ? result : (hl_host_result){.status = HL_STATUS_OK, .value = total};
        if (reply.size != 5 || reply.bytes[0] != TREE_WRITE || get32(reply.bytes + 1) > amount) {
            hl_provider_reply_destroy(&reply);
            return failure(EPROTO);
        }
        uint32_t count = get32(reply.bytes + 1);
        hl_provider_reply_destroy(&reply);
        total += count;
        if (count < amount) break;
    }
    return (hl_host_result){.status = HL_STATUS_OK, .value = total};
}

static hl_host_result tree_write_at(void *context, hl_host_handle file, uint64_t offset, hl_host_const_bytes input) {
    uint64_t remote;
    if (!is_tree(file)) return underlying->write_at(underlying_context, file, offset, input);
    (void)context;
    pthread_mutex_lock(&lock);
    tree_slot *slot = lookup(file);
    remote = slot == NULL ? 0 : slot->remote;
    pthread_mutex_unlock(&lock);
    return remote == 0 ? failure(EBADF) : write_at_remote(remote, offset, input);
}

static hl_host_result tree_read(void *context, hl_host_handle file, void *output, uint64_t output_size) {
    hl_host_result result;
    if (!is_tree(file)) return underlying->read(underlying_context, file, output, output_size);
    if (output_size > SIZE_MAX) return failure(EINVAL);
    pthread_mutex_lock(&lock);
    tree_slot *slot = lookup(file);
    if (slot == NULL || (slot->access & HL_HOST_FILE_READ) == 0) {
        pthread_mutex_unlock(&lock);
        return failure(EBADF);
    }
    result = read_at_remote(slot->remote, slot->offset, (hl_host_bytes){.data = output, .size = (size_t)output_size});
    if (result.status == HL_STATUS_OK) slot->offset += result.value;
    pthread_mutex_unlock(&lock);
    (void)context;
    return result;
}

static hl_host_result append_remote(uint64_t remote, hl_host_const_bytes input, uint64_t *end) {
    unsigned char payload[TREE_PAYLOAD];
    size_t total = 0;
    uint64_t final = 0;
    while (total < input.size) {
        hl_provider_reply reply;
        hl_host_result result;
        uint32_t amount = (uint32_t)((input.size - total) > TREE_APPEND_DATA ? TREE_APPEND_DATA : input.size - total);
        payload[0] = TREE_APPEND;
        put64(payload + 1, remote);
        put32(payload + 9, amount);
        memcpy(payload + 13, (const unsigned char *)input.data + total, amount);
        result = request(payload, 13u + amount, &reply);
        if (result.status != HL_STATUS_OK)
            return total == 0 ? result : (hl_host_result){.status = HL_STATUS_OK, .value = total};
        if (reply.size != 13 || reply.bytes[0] != TREE_APPEND || get32(reply.bytes + 1) > amount) {
            hl_provider_reply_destroy(&reply);
            return failure(EPROTO);
        }
        uint32_t count = get32(reply.bytes + 1);
        final = get64(reply.bytes + 5);
        hl_provider_reply_destroy(&reply);
        total += count;
        if (count < amount) break;
    }
    *end = final;
    return (hl_host_result){.status = HL_STATUS_OK, .value = total};
}

static hl_host_result tree_write(void *context, hl_host_handle file, const void *input, uint64_t input_size) {
    hl_host_result result;
    uint64_t end = 0;
    if (!is_tree(file)) return underlying->write(underlying_context, file, input, input_size);
    if (input_size > SIZE_MAX) return failure(EINVAL);
    pthread_mutex_lock(&lock);
    tree_slot *slot = lookup(file);
    if (slot == NULL || (slot->access & HL_HOST_FILE_WRITE) == 0) {
        pthread_mutex_unlock(&lock);
        return failure(EBADF);
    }
    hl_host_const_bytes bytes = {.data = input, .size = (size_t)input_size};
    if ((slot->access & HL_HOST_FILE_APPEND) != 0) {
        result = append_remote(slot->remote, bytes, &end);
        if (result.status == HL_STATUS_OK) slot->offset = end;
    } else {
        result = write_at_remote(slot->remote, slot->offset, bytes);
        if (result.status == HL_STATUS_OK) slot->offset += result.value;
    }
    pthread_mutex_unlock(&lock);
    (void)context;
    return result;
}

static hl_host_result tree_append(void *context, hl_host_handle file, hl_host_const_bytes input) {
    hl_host_result result;
    uint64_t end = 0;
    if (!is_tree(file)) return underlying->append(underlying_context, file, input);
    pthread_mutex_lock(&lock);
    tree_slot *slot = lookup(file);
    if (slot == NULL || (slot->access & HL_HOST_FILE_WRITE) == 0) {
        pthread_mutex_unlock(&lock);
        return failure(EBADF);
    }
    result = append_remote(slot->remote, input, &end);
    if (result.status == HL_STATUS_OK) slot->offset = end;
    pthread_mutex_unlock(&lock);
    (void)context;
    return result;
}

static hl_host_result tree_metadata(void *context, hl_host_handle file, hl_host_file_metadata *output) {
    unsigned char payload[9] = {TREE_STAT};
    hl_provider_reply reply;
    hl_host_result result;
    uint64_t remote;
    if (!is_tree(file)) return underlying->metadata(underlying_context, file, output);
    if (output == NULL) return failure(EINVAL);
    pthread_mutex_lock(&lock);
    tree_slot *slot = lookup(file);
    remote = slot == NULL ? 0 : slot->remote;
    pthread_mutex_unlock(&lock);
    if (remote == 0) return failure(EBADF);
    put64(payload + 1, remote);
    result = request(payload, sizeof(payload), &reply);
    if (result.status == HL_STATUS_OK && (reply.size != 29 || reply.bytes[0] != TREE_STAT)) result = failure(EPROTO);
    if (result.status == HL_STATUS_OK) {
        uint32_t mode = get32(reply.bytes + 9);
        memset(output, 0, sizeof(*output));
        output->size = get64(reply.bytes + 1);
        output->allocated_size = output->size;
        output->permissions = mode & 07777u;
        output->stable_device = get64(reply.bytes + 13);
        output->stable_object = get64(reply.bytes + 21);
        output->device = output->stable_device;
        output->link_count = 1;
        output->type = (mode & 0170000u) == 0040000u   ? HL_HOST_FILE_TYPE_DIRECTORY
                       : (mode & 0170000u) == 0120000u ? HL_HOST_FILE_TYPE_SYMLINK
                                                       : HL_HOST_FILE_TYPE_REGULAR;
    }
    hl_provider_reply_destroy(&reply);
    (void)context;
    return result;
}

static hl_host_result tree_seek(void *context, hl_host_handle file, int64_t offset, uint32_t whence) {
    hl_host_result result = {.status = HL_STATUS_OK};
    uint64_t base;
    if (!is_tree(file)) return underlying->seek(underlying_context, file, offset, whence);
    pthread_mutex_lock(&lock);
    tree_slot *slot = lookup(file);
    if (slot == NULL) {
        pthread_mutex_unlock(&lock);
        return failure(EBADF);
    }
    if (whence == HL_HOST_FILE_SEEK_SET)
        base = 0;
    else if (whence == HL_HOST_FILE_SEEK_CUR)
        base = slot->offset;
    else if (whence == HL_HOST_FILE_SEEK_END) {
        hl_host_file_metadata metadata;
        pthread_mutex_unlock(&lock);
        result = tree_metadata(context, file, &metadata);
        if (result.status != HL_STATUS_OK) return result;
        pthread_mutex_lock(&lock);
        slot = lookup(file);
        if (slot == NULL) {
            pthread_mutex_unlock(&lock);
            return failure(EBADF);
        }
        base = metadata.size;
    } else {
        pthread_mutex_unlock(&lock);
        return failure(EINVAL);
    }
    if ((offset < 0 && (uint64_t)(-(offset + 1)) + 1 > base) || (offset >= 0 && (uint64_t)offset > UINT64_MAX - base)) {
        pthread_mutex_unlock(&lock);
        return failure(EINVAL);
    }
    slot->offset = offset < 0 ? base - ((uint64_t)(-(offset + 1)) + 1) : base + (uint64_t)offset;
    result.value = slot->offset;
    pthread_mutex_unlock(&lock);
    return result;
}

static hl_host_result tree_clone(void *context, hl_host_handle file) {
    if (!is_tree(file)) return underlying->clone_for_fork(underlying_context, file);
    pthread_mutex_lock(&lock);
    tree_slot *slot = lookup(file);
    if (slot == NULL || slot->references == UINT32_MAX) {
        pthread_mutex_unlock(&lock);
        return failure(slot == NULL ? EBADF : EMFILE);
    }
    slot->references++;
    pthread_mutex_unlock(&lock);
    (void)context;
    return (hl_host_result){.status = HL_STATUS_OK, .value = file};
}

static hl_host_result tree_close(void *context, hl_host_handle file) {
    unsigned char payload[9] = {TREE_CLOSE};
    hl_provider_reply reply;
    hl_host_result result;
    uint64_t remote;
    if (!is_tree(file)) return underlying->close(underlying_context, file);
    pthread_mutex_lock(&lock);
    tree_slot *slot = lookup(file);
    if (slot == NULL) {
        pthread_mutex_unlock(&lock);
        return failure(EBADF);
    }
    if (--slot->references != 0) {
        pthread_mutex_unlock(&lock);
        return (hl_host_result){.status = HL_STATUS_OK};
    }
    remote = slot->remote;
    memset(slot, 0, offsetof(tree_slot, generation));
    slot->live = 0;
    pthread_mutex_unlock(&lock);
    put64(payload + 1, remote);
    result = request(payload, sizeof(payload), &reply);
    if (result.status == HL_STATUS_OK && (reply.size != 1 || reply.bytes[0] != TREE_CLOSE)) result = failure(EPROTO);
    hl_provider_reply_destroy(&reply);
    (void)context;
    return result;
}

static hl_host_result tree_truncate(void *context, hl_host_handle file, uint64_t size) {
    unsigned char payload[17] = {TREE_TRUNCATE};
    hl_provider_reply reply;
    hl_host_result result;
    uint64_t remote;
    if (!is_tree(file)) return underlying->truncate(underlying_context, file, size);
    pthread_mutex_lock(&lock);
    tree_slot *slot = lookup(file);
    remote = slot == NULL ? 0 : slot->remote;
    pthread_mutex_unlock(&lock);
    if (remote == 0) return failure(EBADF);
    put64(payload + 1, remote);
    put64(payload + 9, size);
    result = request(payload, sizeof(payload), &reply);
    if (result.status == HL_STATUS_OK && (reply.size != 1 || reply.bytes[0] != TREE_TRUNCATE)) result = failure(EPROTO);
    hl_provider_reply_destroy(&reply);
    (void)context;
    return result;
}

static hl_host_result tree_readlink(void *context, hl_host_handle file, hl_host_bytes output) {
    unsigned char payload[13] = {TREE_LINK};
    hl_provider_reply reply;
    hl_host_result result;
    uint64_t remote;
    if (!is_tree(file)) return underlying->readlink(underlying_context, file, output);
    pthread_mutex_lock(&lock);
    tree_slot *slot = lookup(file);
    remote = slot == NULL ? 0 : slot->remote;
    pthread_mutex_unlock(&lock);
    if (remote == 0 || output.size > UINT32_MAX) return failure(EINVAL);
    uint32_t maximum = (uint32_t)(output.size > TREE_DATA ? TREE_DATA : output.size);
    put64(payload + 1, remote);
    put32(payload + 9, maximum);
    result = request(payload, sizeof(payload), &reply);
    if (result.status == HL_STATUS_OK &&
        (reply.size < 5 || reply.bytes[0] != TREE_LINK || get32(reply.bytes + 1) > maximum ||
         reply.size != 5u + get32(reply.bytes + 1)))
        result = failure(EPROTO);
    if (result.status == HL_STATUS_OK) {
        result.value = get32(reply.bytes + 1);
        memcpy(output.data, reply.bytes + 5, (size_t)result.value);
    }
    hl_provider_reply_destroy(&reply);
    (void)context;
    return result;
}

static hl_host_result tree_read_directory(void *context, hl_host_handle file, hl_host_file_entry *entries,
                                          uint32_t entry_capacity, uint32_t byte_capacity) {
    unsigned char payload[13] = {TREE_DENTS};
    hl_provider_reply reply;
    hl_host_result result;
    uint64_t remote;
    uint32_t count = 0;
    if (!is_tree(file))
        return underlying->read_directory(underlying_context, file, entries, entry_capacity, byte_capacity);
    if (entries == NULL || entry_capacity == 0) return failure(EINVAL);
    pthread_mutex_lock(&lock);
    tree_slot *slot = lookup(file);
    remote = slot == NULL ? 0 : slot->remote;
    pthread_mutex_unlock(&lock);
    if (remote == 0) return failure(EBADF);
    uint32_t maximum = byte_capacity > TREE_DATA ? TREE_DATA : byte_capacity;
    put64(payload + 1, remote);
    put32(payload + 9, maximum);
    result = request(payload, sizeof(payload), &reply);
    if (result.status == HL_STATUS_OK &&
        (reply.size < 5 || reply.bytes[0] != TREE_DENTS || get32(reply.bytes + 1) > maximum ||
         reply.size != 5u + get32(reply.bytes + 1)))
        result = failure(EPROTO);
    if (result.status == HL_STATUS_OK) {
        const unsigned char *bytes = reply.bytes + 5;
        uint32_t size = get32(reply.bytes + 1);
        uint32_t offset = 0;
        while (offset < size && count < entry_capacity) {
            if (size - offset < 20) {
                result = failure(EPROTO);
                break;
            }
            uint16_t record = get16(bytes + offset + 16);
            if (record < 20 || record > size - offset) {
                result = failure(EPROTO);
                break;
            }
            const unsigned char *name = bytes + offset + 19;
            size_t available = record - 19u;
            const unsigned char *end = memchr(name, 0, available);
            if (end == NULL || (size_t)(end - name) > 255) {
                result = failure(EPROTO);
                break;
            }
            entries[count].object = get64(bytes + offset);
            entries[count].next_offset = get64(bytes + offset + 8);
            entries[count].type = bytes[offset + 18];
            entries[count].name_size = (uint32_t)(end - name);
            memcpy(entries[count].name, name, entries[count].name_size);
            count++;
            offset += record;
        }
        if (result.status == HL_STATUS_OK && offset != size) result = failure(EPROTO);
    }
    if (result.status == HL_STATUS_OK) result.value = count;
    hl_provider_reply_destroy(&reply);
    (void)context;
    return result;
}

static hl_host_result tree_vectors(void *context, hl_host_handle file, const hl_host_iovec *vectors, uint32_t count,
                                   uint64_t offset, int writing, int positioned) {
    uint64_t total = 0;
    for (uint32_t index = 0; index < count; ++index) {
        hl_host_result result;
        uint64_t at = positioned ? offset + total : 0;
        if (writing) {
            hl_host_const_bytes input = {.data = (const void *)(uintptr_t)vectors[index].address,
                                         .size = (size_t)vectors[index].size};
            result = positioned ? tree_write_at(context, file, at, input)
                                : tree_write(context, file, input.data, input.size);
        } else {
            hl_host_bytes output = {.data = (void *)(uintptr_t)vectors[index].address,
                                    .size = (size_t)vectors[index].size};
            result = positioned ? tree_read_at(context, file, at, output)
                                : tree_read(context, file, output.data, output.size);
        }
        if (result.status != HL_STATUS_OK)
            return total == 0 ? result : (hl_host_result){.status = HL_STATUS_OK, .value = total};
        total += result.value;
        if (result.value < vectors[index].size) break;
    }
    return (hl_host_result){.status = HL_STATUS_OK, .value = total};
}

static hl_host_result tree_readv(void *context, hl_host_handle file, const hl_host_iovec *vectors, uint32_t count) {
    if (!is_tree(file)) return underlying->readv(underlying_context, file, vectors, count);
    return tree_vectors(context, file, vectors, count, 0, 0, 0);
}

static hl_host_result tree_writev(void *context, hl_host_handle file, const hl_host_iovec *vectors, uint32_t count) {
    if (!is_tree(file)) return underlying->writev(underlying_context, file, vectors, count);
    return tree_vectors(context, file, vectors, count, 0, 1, 0);
}

static hl_host_result tree_readv_at(void *context, hl_host_handle file, const hl_host_iovec *vectors, uint32_t count,
                                    uint64_t offset) {
    if (!is_tree(file)) return underlying->readv_at(underlying_context, file, vectors, count, offset);
    return tree_vectors(context, file, vectors, count, offset, 0, 1);
}

static hl_host_result tree_writev_at(void *context, hl_host_handle file, const hl_host_iovec *vectors, uint32_t count,
                                     uint64_t offset) {
    if (!is_tree(file)) return underlying->writev_at(underlying_context, file, vectors, count, offset);
    return tree_vectors(context, file, vectors, count, offset, 1, 1);
}

static hl_host_result tree_sync(void *context, hl_host_handle file) {
    if (!is_tree(file)) return underlying->sync(underlying_context, file);
    (void)context;
    return lookup(file) == NULL ? failure(EBADF) : (hl_host_result){.status = HL_STATUS_OK};
}

int hl_provider_tree_files_install(hl_host_services *services, hl_provider_client *client) {
    if (services == NULL || services->file == NULL || client == NULL || provider != NULL) return -EINVAL;
    underlying = services->file;
    underlying_context = services->context;
    provider = client;
    composite = *underlying;
    composite.open_relative = tree_open_relative;
    composite.read_at = tree_read_at;
    composite.write_at = tree_write_at;
    composite.append = tree_append;
    composite.metadata = tree_metadata;
    composite.close = tree_close;
    composite.read = tree_read;
    composite.write = tree_write;
    composite.clone_for_fork = tree_clone;
    composite.seek = tree_seek;
    composite.readv = tree_readv;
    composite.writev = tree_writev;
    composite.readv_at = tree_readv_at;
    composite.writev_at = tree_writev_at;
    composite.truncate = tree_truncate;
    composite.sync = tree_sync;
    composite.data_sync = tree_sync;
    composite.readlink = tree_readlink;
    composite.read_directory = tree_read_directory;
    services->file = &composite;
    return 0;
}

void hl_provider_tree_files_revoke(void) {
    pthread_mutex_lock(&lock);
    memset(slots, 0, sizeof(slots));
    provider = NULL;
    underlying = NULL;
    underlying_context = NULL;
    memset(&composite, 0, sizeof(composite));
    pthread_mutex_unlock(&lock);
}

int hl_provider_tree_files_active(void) {
    return provider != NULL;
}
