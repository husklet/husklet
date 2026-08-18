#include "transport.h"

#include <errno.h>

static int hl_socket_owner_key_valid(hl_owner_key key) {
    return key.birth_ns != 0 && (key.device != 0 || key.object != 0);
}

static int hl_socket_owner_key_equal(hl_owner_key left, hl_owner_key right) {
    return left.device == right.device && left.object == right.object && left.birth_ns == right.birth_ns;
}

uint64_t hl_socket_owner_image_checksum(const hl_socket_owner_image_record *records, size_t count) {
    /* FNV-1a over the fixed-width records.  This is corruption detection, not
     * authentication; the enclosing checkpoint already owns image trust. */
    const uint8_t *bytes = (const uint8_t *)records;
    size_t length = count * sizeof *records;
    uint64_t hash = UINT64_C(14695981039346656037);
    for (size_t index = 0; index < length; ++index) {
        hash ^= bytes[index];
        hash *= UINT64_C(1099511628211);
    }
    return hash;
}

int hl_socket_owner_image_validate(const hl_socket_owner_image_header *header,
                                   const hl_socket_owner_image_record *records, size_t available) {
    if (header == NULL || (records == NULL && header->count != 0)) return EINVAL;
    if (header->magic != HL_SOCKET_OWNER_IMAGE_MAGIC || header->version != HL_SOCKET_OWNER_IMAGE_VERSION ||
        header->record_size != sizeof *records)
        return EPROTO;
    if (header->count > SIZE_MAX / sizeof *records || header->count > available) return EOVERFLOW;
    if (hl_socket_owner_image_checksum(records, (size_t)header->count) != header->checksum) return EBADMSG;
    for (size_t index = 0; index < (size_t)header->count; ++index) {
        const hl_socket_owner_image_record *record = &records[index];
        if (record->object_id == 0 || !hl_socket_owner_key_valid(record->key) || record->descriptors == 0)
            return EINVAL;
        for (size_t prior = 0; prior < index; ++prior)
            if (records[prior].object_id == record->object_id ||
                hl_socket_owner_key_equal(records[prior].key, record->key))
                return EEXIST;
    }
    return 0;
}
