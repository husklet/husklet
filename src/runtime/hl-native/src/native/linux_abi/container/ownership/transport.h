#ifndef HL_LINUX_OWNERSHIP_TRANSPORT_H
#define HL_LINUX_OWNERSHIP_TRANSPORT_H

#include "registry.h"

#include <stddef.h>
#include <stdint.h>

/* Optional payload appended to an SCM_RIGHTS OFD marker.  Version zero means
 * that the sender did not associate the descriptor with a socket owner. */
#define HL_SOCKET_OWNER_TRANSPORT_VERSION 1u

typedef struct hl_socket_owner_transport {
    uint32_t version;
    uint32_t size;
    hl_owner_key key;
} hl_socket_owner_transport;

/* Checkpoint ownership is a side table rather than part of ckpt_fd: a complete
 * key does not fit in ckpt_fd.  There is exactly one record per socket object,
 * and descriptors is the aggregate number of restored aliases for that key. */
#define HL_SOCKET_OWNER_IMAGE_MAGIC UINT64_C(0x484c534f574e5231)
#define HL_SOCKET_OWNER_IMAGE_VERSION 1u

typedef struct hl_socket_owner_image_header {
    uint64_t magic;
    uint32_t version;
    uint32_t record_size;
    uint64_t count;
    uint64_t checksum;
} hl_socket_owner_image_header;

typedef struct hl_socket_owner_image_record {
    uint64_t object_id;
    hl_owner_key key;
    uint32_t uid;
    uint32_t gid;
    uint32_t links;
    uint32_t descriptors;
} hl_socket_owner_image_record;

uint64_t hl_socket_owner_image_checksum(const hl_socket_owner_image_record *records, size_t count);

/* Validates the complete table before restore publishes its first registry
 * entry. Duplicate object ids or keys make the image ambiguous and fail it. */
int hl_socket_owner_image_validate(const hl_socket_owner_image_header *header,
                                   const hl_socket_owner_image_record *records, size_t available);

#endif
