// hl/linux_abi -- the implementation of the checkpoint sink (see ckpt_sink.h).
//
// Every operation becomes one request/response round trip on this process's private channel
// (hl/checkpoint_stream.h, src/core/checkpoint_channel.c). No filesystem is involved anywhere: the
// sink hands the bytes to the embedder and lets the embedder decide what "durable" and "visible" mean.
//
// STAGING AND ATOMICITY are the server's job, not this file's: the ordering contract (an object is complete
// only after finish, a group is invisible until group_commit) is stated in the protocol and implemented once,
// on the Rust side, instead of once per engine process. That is deliberate -- the engine processes cannot
// coordinate with each other, and the server is the only participant that sees all of them.
//
// FAILURE: any transport failure poisons the stream. The writer aborts the object and fails its caller, the
// group is aborted and the process exits non-zero, and the coordinator refuses to publish a manifest. There
// is no silent truncation.

#ifndef HL_LINUX_ABI_CKPT_SINK_STREAM_H
#define HL_LINUX_ABI_CKPT_SINK_STREAM_H

#include "ckpt_sink.h"
#include "../engine/checkpoint_channel.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static uint64_t g_ckpt_stream_next_id = 1;

// One round trip with no payload in either direction. Returns the reply status, or -1 on transport failure.
static int ckpt_stream_call(uint32_t op, const char *name, uint64_t stream, uint64_t offset, uint32_t flags,
                            const void *payload, size_t size, hl_ckpt_reply *reply, void *out, size_t capacity) {
    hl_ckpt_request request = {0};
    hl_ckpt_reply local;
    if (reply == NULL) reply = &local;
    request.op = op;
    request.flags = flags;
    request.stream = stream;
    request.offset = offset;
    request.length = (uint64_t)size;
    request.generation = ckpt_request_generation();
    if (hl_ckpt_channel_call(&request, name, payload, reply, out, capacity) != 0) return -1;
    return reply->status;
}

// Tell the host that this capture has been REFUSED, and by what. Best effort and never load-bearing for
// correctness: the refusal itself is already decided and the coordinator exits regardless. What it buys is
// that the host fails the capture on the decision rather than on its own deadline, and can name the reason.
static void ckpt_stream_capture_refused(const char *reason) {
    if (reason == NULL || reason[0] == 0) reason = "the coordinator refused the capture";
    (void)ckpt_stream_call(HL_CKPT_OP_CAPTURE_REFUSED, reason, 0, 0, 0, NULL, 0, NULL, NULL, 0);
}

// Ask the broker whether `host_pid` ever proved exact membership (REGISTER_READY) of the capture
// generation this process is running. Returns 1 registered, 0 never registered, -1 unknown.
//
// Only a 0 carries information the coordinator may act on, and only about a process that is ALREADY GONE:
// a live peer can still register. -1 and 1 are the same answer to the caller -- do not drop this member --
// which is what keeps a broker that is unreachable, poisoned, or out of scope from being read as consent.
static int ckpt_stream_participant_registered(long long host_pid) {
    uint64_t identity = (uint64_t)host_pid;
    hl_ckpt_reply reply;
    if (host_pid <= 0) return -1;
    if (ckpt_stream_call(HL_CKPT_OP_PARTICIPANT_REGISTERED, NULL, 0, 0, 0, &identity, sizeof identity, &reply, NULL,
                         0) != HL_CKPT_STATUS_OK)
        return -1;
    return reply.value != 0 ? 1 : 0;
}

static int ckpt_stream_recovery_complete(void) {
    return ckpt_stream_call(HL_CKPT_OP_RECOVERY_COMPLETE, NULL, 0, 0, 0, NULL, 0, NULL, NULL, 0) ==
                   HL_CKPT_STATUS_OK
               ? 0
               : -1;
}

// Announce this restored process to the broker under the guest pid the image named it by.
//
// The connection this rides on is the process's OWN channel (hl_ckpt_channel_acquire re-creates one per
// process after every fork), so the broker has already authenticated the caller: this call supplies the
// only thing that authentication cannot, which is which captured member the live process IS. Sent after
// the identity is hydrated and before the commit barrier, so it lands inside the recovery scope.
//
// A refusal is not fatal to the restore. The tree is already correct; only the host's ability to reach
// this member individually is lost, and the host refuses to reattach rather than inventing one.
static int ckpt_stream_member_restored(int guest_pid) {
    unsigned char payload[8] = {0};
    uint32_t encoded = (uint32_t)guest_pid;
    if (guest_pid <= 0) return -1;
    memcpy(payload, &encoded, sizeof encoded);
    return ckpt_stream_call(HL_CKPT_OP_MEMBER_RESTORED, NULL, 0, 0, 0, payload, sizeof payload, NULL, NULL, 0) ==
                   HL_CKPT_STATUS_OK
               ? 0
               : -1;
}

// Ask the broker for the terminal this member's captured session was attached to.
//
// Sent from inside the descriptor restore, which runs BEFORE the identity is hydrated -- so it cannot ride
// the MEMBER_RESTORED announcement and carries the guest pid itself. `g_self_gpid` is adopted at the top of
// ckpt_restore_proc_run, long before any descriptor is rebuilt, so it already holds this member's own
// captured name here.
//
// Returns the received descriptor, or -1 when the host registered no terminal for this member and when the
// transport failed. Both are the same answer to the caller: keep the descriptor the restore would otherwise
// have produced. Never returns a descriptor belonging to another session -- the server answers only for the
// guest pid it was asked about, and hands each registration out once.
static int ckpt_stream_member_stdio(int guest_pid) {
    hl_ckpt_request request = {0};
    hl_ckpt_reply reply;
    unsigned char payload[8] = {0};
    uint32_t encoded = (uint32_t)guest_pid;
    int descriptor = -1;
    if (guest_pid <= 0) return -1;
    memcpy(payload, &encoded, sizeof encoded);
    request.op = HL_CKPT_OP_MEMBER_STDIO;
    request.length = (uint64_t)sizeof payload;
    request.generation = ckpt_request_generation();
    if (hl_ckpt_channel_call_receive_descriptor(&request, payload, &reply, &descriptor) != 0) return -1;
    if (reply.status != HL_CKPT_STATUS_OK && descriptor >= 0) {
        (void)close(descriptor);
        return -1;
    }
    return reply.status == HL_CKPT_STATUS_OK ? descriptor : -1;
}

// Report this restored member's own guest exit status on its way out.
//
// A host holding the member sees the process vanish either way; this is what lets it report the status the
// guest actually produced instead of "gone, cause unknown". Best effort by construction: a member killed
// outright never runs this, which is exactly the case the host must still describe honestly.
static void ckpt_stream_member_exited(int status, uint32_t kind) {
    unsigned char payload[8] = {0};
    int32_t encoded = (int32_t)status;
    memcpy(payload, &encoded, sizeof encoded);
    memcpy(payload + 4, &kind, sizeof kind);
    (void)ckpt_stream_call(HL_CKPT_OP_MEMBER_EXITED, NULL, 0, 0, 0, payload, sizeof payload, NULL, NULL, 0);
}

static void ckpt_sink_stream_name(struct ckpt_sink *sink, const char *group, const char *name, char *out, size_t size) {
    (void)sink;
    if (group)
        snprintf(out, size, "%s/%s", group, name);
    else
        snprintf(out, size, "%s", name);
}

static int ckpt_sink_stream_flush(struct ckpt_sink_stream *stream) {
    if (stream->failed) return -1;
    if (stream->buffered == 0) return 0;
    // Append: the buffer always sits at the object's logical end, because write_at flushes before patching.
    if (ckpt_stream_call(HL_CKPT_OP_OBJECT_WRITE, NULL, stream->id, 0, 0, stream->buffer, stream->buffered, NULL, NULL,
                         0) != HL_CKPT_STATUS_OK) {
        stream->failed = 1;
        return -1;
    }
    stream->buffered = 0;
    return 0;
}

static int ckpt_sink_stream_write(struct ckpt_sink_stream *stream, const void *data, size_t size) {
    if (stream->failed) return -1;
    const unsigned char *bytes = data;
    while (size != 0) {
        if (stream->buffered == CKPT_SINK_BUFFER && ckpt_sink_stream_flush(stream) != 0) return -1;
        size_t room = CKPT_SINK_BUFFER - stream->buffered;
        size_t chunk = size < room ? size : room;
        memcpy(stream->buffer + stream->buffered, bytes, chunk);
        stream->buffered += chunk;
        stream->position += chunk;
        bytes += chunk;
        size -= chunk;
    }
    return 0;
}

static int ckpt_sink_stream_write_at(struct ckpt_sink_stream *stream, uint64_t offset, const void *data, size_t size) {
    if (stream->failed || ckpt_sink_stream_flush(stream) != 0) return -1;
    if (ckpt_stream_call(HL_CKPT_OP_OBJECT_WRITE_AT, NULL, stream->id, offset, 0, data, size, NULL, NULL, 0) !=
        HL_CKPT_STATUS_OK) {
        stream->failed = 1;
        return -1;
    }
    if (offset + size > stream->position) stream->position = offset + size;
    return 0;
}

static int64_t ckpt_sink_stream_tell(struct ckpt_sink_stream *stream) {
    // The write cursor is authoritative locally: buffered bytes are already accounted for in `position`, and
    // the server never appends on its own. No round trip is needed, and none is made.
    return stream->failed ? -1 : (int64_t)stream->position;
}

static int ckpt_sink_stream_finish(struct ckpt_sink_stream *stream) {
    int failed = stream->failed || ckpt_sink_stream_flush(stream) != 0;
    if (!failed &&
        ckpt_stream_call(HL_CKPT_OP_OBJECT_FINISH, NULL, stream->id, 0, 0, NULL, 0, NULL, NULL, 0) != HL_CKPT_STATUS_OK)
        failed = 1;
    if (failed) (void)ckpt_stream_call(HL_CKPT_OP_OBJECT_ABORT, NULL, stream->id, 0, 0, NULL, 0, NULL, NULL, 0);
    free(stream);
    return failed ? -1 : 0;
}

static void ckpt_sink_stream_abort(struct ckpt_sink_stream *stream) {
    (void)ckpt_stream_call(HL_CKPT_OP_OBJECT_ABORT, NULL, stream->id, 0, 0, NULL, 0, NULL, NULL, 0);
    free(stream);
}

static int ckpt_sink_stream_begin(struct ckpt_sink *sink, const char *group, const char *name, uint32_t flags,
                                  struct ckpt_sink_stream **out) {
    char object[HL_CKPT_STREAM_NAME_MAX];
    struct ckpt_sink_stream *stream = calloc(1, sizeof *stream);
    if (!stream) return -1;
    ckpt_sink_stream_name(sink, group, name, object, sizeof object);
    stream->sink = sink;
    stream->flags = flags;
    stream->id = g_ckpt_stream_next_id++;
    if (ckpt_stream_call(HL_CKPT_OP_OBJECT_BEGIN, object, stream->id, 0, flags, NULL, 0, NULL, NULL, 0) !=
        HL_CKPT_STATUS_OK) {
        free(stream);
        return -1;
    }
    *out = stream;
    return 0;
}

static int ckpt_sink_stream_group_begin(struct ckpt_sink *sink, const char *group) {
    (void)sink;
    return ckpt_stream_call(HL_CKPT_OP_GROUP_BEGIN, group, 0, 0, 0, NULL, 0, NULL, NULL, 0) == HL_CKPT_STATUS_OK ? 0
                                                                                                                 : -1;
}

static int ckpt_sink_stream_group_commit(struct ckpt_sink *sink, const char *group) {
    (void)sink;
    return ckpt_stream_call(HL_CKPT_OP_GROUP_COMMIT, group, 0, 0, 0, NULL, 0, NULL, NULL, 0) == HL_CKPT_STATUS_OK ? 0
                                                                                                                  : -1;
}

static void ckpt_sink_stream_group_abort(struct ckpt_sink *sink, const char *group) {
    (void)sink;
    (void)ckpt_stream_call(HL_CKPT_OP_GROUP_ABORT, group, 0, 0, 0, NULL, 0, NULL, NULL, 0);
}

static int ckpt_sink_stream_claim(struct ckpt_sink *sink, const char *name) {
    (void)sink;
    // The server owns the claim table, so election is decided in one place for the whole process tree --
    // exactly the role O_EXCL played when the store was a directory.
    int status = ckpt_stream_call(HL_CKPT_OP_CLAIM, name, 0, 0, 0, NULL, 0, NULL, NULL, 0);
    return status == HL_CKPT_STATUS_ALREADY ? 1 : status == HL_CKPT_STATUS_OK ? 0 : -1;
}

static void ckpt_sink_stream_unclaim(struct ckpt_sink *sink, const char *name) {
    (void)sink;
    (void)ckpt_stream_call(HL_CKPT_OP_UNCLAIM, name, 0, 0, 0, NULL, 0, NULL, NULL, 0);
}

static int ckpt_sink_stream_group_present(struct ckpt_sink *sink, const char *group) {
    hl_ckpt_reply reply;
    (void)sink;
    if (ckpt_stream_call(HL_CKPT_OP_GROUP_PRESENT, group, 0, 0, 0, NULL, 0, &reply, NULL, 0) != HL_CKPT_STATUS_OK)
        return -1;
    return reply.value != 0 ? 1 : 0;
}

static int ckpt_sink_stream_group_count(struct ckpt_sink *sink, const char *prefix) {
    hl_ckpt_reply reply;
    (void)sink;
    if (ckpt_stream_call(HL_CKPT_OP_GROUP_COUNT, prefix, 0, 0, 0, NULL, 0, &reply, NULL, 0) != HL_CKPT_STATUS_OK)
        return -1;
    return (int)reply.value;
}

static int ckpt_sink_stream_digest(struct ckpt_sink *sink, uint64_t *hash, uint64_t *files, uint64_t *bytes) {
    hl_ckpt_stream_digest digest = {0};
    hl_ckpt_reply reply;
    (void)sink;
    if (ckpt_stream_call(HL_CKPT_OP_DIGEST, NULL, 0, 0, 0, NULL, 0, &reply, &digest, sizeof digest) !=
            HL_CKPT_STATUS_OK ||
        reply.length != sizeof digest)
        return -1;
    *hash = digest.hash;
    *files = digest.files;
    *bytes = digest.bytes;
    return 0;
}

static int ckpt_sink_stream_commit(struct ckpt_sink *sink, const void *manifest, size_t size) {
    (void)sink;
    return ckpt_stream_call(HL_CKPT_OP_COMMIT, NULL, 0, 0, 0, manifest, size, NULL, NULL, 0) == HL_CKPT_STATUS_OK ? 0
                                                                                                                  : -1;
}

static const ckpt_sink_vtable g_ckpt_sink_stream_ops = {
    .begin = ckpt_sink_stream_begin,
    .write = ckpt_sink_stream_write,
    .write_at = ckpt_sink_stream_write_at,
    .tell = ckpt_sink_stream_tell,
    .finish = ckpt_sink_stream_finish,
    .abort = ckpt_sink_stream_abort,
    .group_begin = ckpt_sink_stream_group_begin,
    .group_commit = ckpt_sink_stream_group_commit,
    .group_abort = ckpt_sink_stream_group_abort,
    .claim = ckpt_sink_stream_claim,
    .unclaim = ckpt_sink_stream_unclaim,
    .group_present = ckpt_sink_stream_group_present,
    .group_count = ckpt_sink_stream_group_count,
    .digest = ckpt_sink_stream_digest,
    .commit = ckpt_sink_stream_commit,
};

// Bind the process-wide checkpoint sink to the embedder's store. Fails (NULL) when no broker descriptor was
// inherited from activation, which is the only honest outcome: there is nowhere to put the bytes.
static struct ckpt_sink *ckpt_sink_bind_stream(void) {
    if (hl_ckpt_channel_broker() < 0) return NULL;
    return ckpt_sink_install(&g_ckpt_sink_stream_ops);
}

#endif
