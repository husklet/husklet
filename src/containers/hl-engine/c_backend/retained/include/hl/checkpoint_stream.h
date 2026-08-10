/* hl/checkpoint_stream.h -- the wire protocol that carries checkpoint image bytes out of the engine
 * process tree and into an embedder-supplied store.
 *
 * WHY A WIRE PROTOCOL AT ALL: the checkpoint sink (src/linux_abi/ckpt_sink.h) is an in-process vtable, but
 * the bytes are produced by N separate host processes -- hl_activation_start_* re-executes a separate engine
 * executable and every guest process is a further fork() of it. None of them shares an address space with the
 * embedder. So the sink vtable is implemented, in each engine process, by a client that marshals every
 * operation onto a descriptor inherited from activation, and the embedder runs a server that replays them
 * onto its own storage.
 *
 * TOPOLOGY
 *   broker    : ONE SOCK_SEQPACKET descriptor, handed to the first engine process by activation (SCM_RIGHTS)
 *               and inherited by every fork(). It carries exactly one message kind, hl_ckpt_hello, with one
 *               attached descriptor: the sending process's private channel. Datagram framing makes a
 *               concurrent sendmsg from an arbitrary number of engine processes atomic, and nothing is ever
 *               read from it by the engine, so there is no cross-process response ambiguity.
 *   channel   : ONE SOCK_STREAM descriptor per engine process, created by that process and passed to the
 *               server over the broker. Strictly request/response, strictly serial: an engine process has one
 *               thread at a safepoint when it dumps. Concurrency between PROCESSES is therefore demultiplexed
 *               by the server having one channel per process -- there are no request tags to match, because
 *               there is never more than one request outstanding on a channel.
 *
 * VERSIONING: HL_CKPT_STREAM_ABI is checked on both sides of the very first message of every channel and of
 * the broker hello. A mismatch fails the capture rather than producing an image that cannot be read back;
 * this codebase has already shipped one incident caused by a silently tolerated checkpoint format skew.
 */

#ifndef HL_CHECKPOINT_STREAM_H
#define HL_CHECKPOINT_STREAM_H

#include "base.h"

HL_EXTERN_C_BEGIN

#define HL_CKPT_STREAM_MAGIC_HELLO UINT32_C(0x484b4348)   /* "HKCH" */
#define HL_CKPT_STREAM_MAGIC_REQUEST UINT32_C(0x484b4351) /* "HKCQ" */
#define HL_CKPT_STREAM_MAGIC_REPLY UINT32_C(0x484b4353)   /* "HKCS" */
#define HL_CKPT_STREAM_ABI 1u

/* Largest object name the protocol will carry, and the largest payload in a single request. Both are
 * enforced on both sides; a hostile or corrupt peer can never make the other end allocate without bound. */
#define HL_CKPT_STREAM_NAME_MAX 512u
#define HL_CKPT_STREAM_PAYLOAD_MAX (4u * 1024u * 1024u)

typedef enum hl_ckpt_stream_op {
    /* --- sink (capture) --- */
    HL_CKPT_OP_OBJECT_BEGIN = 1,    /* name, flags -> ok            : opens `stream` */
    HL_CKPT_OP_OBJECT_WRITE = 2,    /* stream, payload -> ok        : append */
    HL_CKPT_OP_OBJECT_WRITE_AT = 3, /* stream, offset, payload -> ok: patch already-emitted bytes */
    HL_CKPT_OP_OBJECT_TELL = 4,     /* stream -> value = logical end */
    HL_CKPT_OP_OBJECT_FINISH = 5,   /* stream -> ok                 : object complete and visible */
    HL_CKPT_OP_OBJECT_ABORT = 6,    /* stream -> ok                 : object discarded */
    HL_CKPT_OP_GROUP_BEGIN = 7,     /* name -> ok */
    HL_CKPT_OP_GROUP_COMMIT = 8,    /* name -> ok                   : the group becomes visible */
    HL_CKPT_OP_GROUP_ABORT = 9,     /* name -> ok */
    HL_CKPT_OP_CLAIM = 10,          /* name -> status 0 acquired / 1 already held */
    HL_CKPT_OP_UNCLAIM = 11,        /* name -> ok */
    HL_CKPT_OP_COMMIT = 12,         /* payload = manifest -> ok     : the image is complete */
    /* --- rendezvous: a peer is finished only once its group is committed --- */
    HL_CKPT_OP_GROUP_PRESENT = 13, /* name -> value = 1 when that group has been committed */
    HL_CKPT_OP_GROUP_COUNT = 14,   /* name = prefix -> value = committed groups with that prefix */
    /* --- digest (accumulated as bytes are emitted; never re-reads the store) --- */
    HL_CKPT_OP_DIGEST = 15, /* -> payload = hl_ckpt_stream_digest */
    /* --- source (restore) --- */
    HL_CKPT_OP_SOURCE_LIST = 16, /* name = prefix -> payload = NUL-terminated names, value = count */
    HL_CKPT_OP_SOURCE_SIZE = 17, /* name -> value = size, status 1 when absent */
    HL_CKPT_OP_SOURCE_READ = 18  /* name, offset, length -> payload (short read at end of object) */
} hl_ckpt_stream_op;

/* Announces one engine process on the broker. Carries exactly one descriptor: that process's channel. */
typedef struct hl_ckpt_hello {
    uint32_t magic;
    uint32_t abi;
    uint64_t host_pid; /* diagnostic only; the server keys on the channel, not on this */
} hl_ckpt_hello;

typedef struct hl_ckpt_request {
    uint32_t magic;
    uint32_t abi;
    uint32_t op;
    uint32_t flags;     /* CKPT_SINK_PUBLISH_ATOMIC and friends, for OBJECT_BEGIN */
    uint64_t stream;    /* engine-assigned object handle, unique within a channel */
    uint64_t offset;    /* WRITE_AT / SOURCE_READ */
    uint64_t length;    /* payload bytes following the name, or requested read length */
    uint32_t name_size; /* including the terminating NUL; 0 when the op takes no name */
    uint32_t reserved;
} hl_ckpt_request;

#define HL_CKPT_STATUS_OK 0
#define HL_CKPT_STATUS_ERROR (-1)
#define HL_CKPT_STATUS_ALREADY 1 /* claim lost the race / queried object absent -- NOT a failure */

typedef struct hl_ckpt_reply {
    uint32_t magic;
    uint32_t abi;
    int32_t status;
    uint32_t reserved;
    uint64_t value;  /* tell / count / size */
    uint64_t length; /* payload bytes following */
} hl_ckpt_reply;

/* The image digest, accumulated while bytes are emitted rather than by walking the finished store. */
typedef struct hl_ckpt_stream_digest {
    uint64_t hash;
    uint64_t files;
    uint64_t bytes;
} hl_ckpt_stream_digest;

HL_EXTERN_C_END

#endif
