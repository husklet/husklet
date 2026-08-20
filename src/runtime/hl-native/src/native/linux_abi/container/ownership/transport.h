#ifndef HL_LINUX_OWNERSHIP_TRANSPORT_H
#define HL_LINUX_OWNERSHIP_TRANSPORT_H

#include "registry.h"

#include <stddef.h>
#include <stdint.h>

/* All persisted fields are little-endian fixed-width integers. Husklet's
 * supported checkpoint architectures are little-endian; cross-endian images
 * are rejected by the enclosing checkpoint architecture contract. */
#if defined(__BYTE_ORDER__) && __BYTE_ORDER__ != __ORDER_LITTLE_ENDIAN__
#error "socket owner transport requires little-endian encoding"
#endif

/* Optional payload stored at a fixed offset after an SCM_RIGHTS OFD marker. */
#define HL_SOCKET_OWNER_TRANSPORT_VERSION 1u
#define HL_SOCKET_OWNER_TRANSPORT_MAGIC UINT64_C(0x484c534f574e5431)
/* The OFD marker prefix is an immutable 16-byte native ABI. Darwin writes its
 * delivery ACK at byte 16; leave the following seven bytes reserved forever. */
#define HL_SOCKET_OWNER_OFD_ACK_OFFSET 16u
#define HL_SOCKET_OWNER_OFD_EXTENSION_OFFSET 24u

typedef struct hl_socket_owner_transport {
    uint64_t magic;
    uint32_t version;
    uint32_t size;
    hl_owner_key key;
} hl_socket_owner_transport;

_Static_assert(sizeof(hl_owner_key) == 24, "owner key wire width");
_Static_assert(offsetof(hl_socket_owner_transport, key) == 16, "owner extension key offset");
_Static_assert(sizeof(hl_socket_owner_transport) == 40, "owner extension wire width");

int hl_socket_owner_transport_valid(const hl_socket_owner_transport *transport);
/* Returns 1 for a valid extension, 0 for a legacy or truncated marker, and a
 * negative errno for a present but malformed extension. */
int hl_socket_owner_transport_decode(const void *marker, size_t marker_size, hl_owner_key *key);

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

_Static_assert(sizeof(hl_socket_owner_image_header) == 32, "owner image header wire width");
_Static_assert(offsetof(hl_socket_owner_image_header, checksum) == 24, "owner image checksum offset");
_Static_assert(sizeof(hl_socket_owner_image_record) == 48, "owner image record wire width");
_Static_assert(offsetof(hl_socket_owner_image_record, key) == 8, "owner image key offset");
_Static_assert(offsetof(hl_socket_owner_image_record, descriptors) == 44, "owner image descriptor offset");

uint64_t hl_socket_owner_image_checksum(const hl_socket_owner_image_record *records, size_t count);
int hl_socket_owner_image_checksum_checked(const hl_socket_owner_image_record *records, size_t count,
                                           uint64_t *checksum);

/* Validates the complete table before restore publishes its first registry
 * entry. Duplicate object ids or keys make the image ambiguous and fail it. */
int hl_socket_owner_image_validate(const hl_socket_owner_image_header *header,
                                   const hl_socket_owner_image_record *records, size_t available);

#endif
