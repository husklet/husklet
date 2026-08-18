#include "transport.h"

#include <errno.h>
#include <string.h>

static int hl_socket_owner_key_valid(hl_owner_key key) {
    return key.birth_ns != 0 && (key.device != 0 || key.object != 0);
}

static int hl_socket_owner_key_equal(hl_owner_key left, hl_owner_key right) {
    return left.device == right.device && left.object == right.object && left.birth_ns == right.birth_ns;
}

int hl_socket_owner_transport_valid(const hl_socket_owner_transport *transport) {
    return transport != NULL && transport->magic == HL_SOCKET_OWNER_TRANSPORT_MAGIC &&
           transport->version == HL_SOCKET_OWNER_TRANSPORT_VERSION && transport->size == sizeof *transport &&
           hl_socket_owner_key_valid(transport->key);
}

int hl_socket_owner_transport_decode(const void *marker, size_t marker_size, hl_owner_key *key) {
    if (marker == NULL || key == NULL) return -EINVAL;
    if (marker_size < HL_SOCKET_OWNER_OFD_EXTENSION_OFFSET + sizeof(hl_socket_owner_transport)) return 0;
    hl_socket_owner_transport transport;
    memcpy(&transport, (const uint8_t *)marker + HL_SOCKET_OWNER_OFD_EXTENSION_OFFSET, sizeof transport);
    if (transport.magic != HL_SOCKET_OWNER_TRANSPORT_MAGIC) return 0;
    if (!hl_socket_owner_transport_valid(&transport)) return -EPROTO;
    *key = transport.key;
    return 1;
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

int hl_socket_owner_image_checksum_checked(const hl_socket_owner_image_record *records, size_t count,
                                           uint64_t *checksum) {
    if (checksum == NULL || (records == NULL && count != 0)) return EINVAL;
    if (count > SIZE_MAX / sizeof *records) return EOVERFLOW;
    *checksum = hl_socket_owner_image_checksum(records, count);
    return 0;
}

int hl_socket_owner_image_validate(const hl_socket_owner_image_header *header,
                                   const hl_socket_owner_image_record *records, size_t available) {
    if (header == NULL || (records == NULL && header->count != 0)) return EINVAL;
    if (header->magic != HL_SOCKET_OWNER_IMAGE_MAGIC || header->version != HL_SOCKET_OWNER_IMAGE_VERSION ||
        header->record_size != sizeof *records)
        return EPROTO;
    if (header->count > SIZE_MAX / sizeof *records || header->count > available) return EOVERFLOW;
    uint64_t checksum;
    int error = hl_socket_owner_image_checksum_checked(records, (size_t)header->count, &checksum);
    if (error != 0) return error;
    if (checksum != header->checksum) return EBADMSG;
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
