// hl/linux_abi -- checkpoint SINK: the only way the checkpoint writer is allowed to emit image bytes.
//
// There is exactly one implementation (ckpt_sink_stream.h): every byte leaves the engine over the socket
// activation handed it, and the embedder decides what it is written to. The vtable stays because the writer
// is large and this is the seam that keeps "what a checkpoint is" out of it.
//
// MODEL
//   object  : a named byte stream ("MANIFEST", "pipe.00000000deadbeef", "pages", ...). begin -> write* ->
//             finish. finish is the point at which the object becomes durable and visible under its name.
//   group   : a set of objects published all-or-nothing under a common prefix -- exactly today's per-process
//             "proc.<gpid>" image. begin/commit/abort.
//   claim   : an image-wide exclusive marker used to elect ONE writer for a shared object (a pipe, a
//             socketpair queue) that several engine processes can see. Returns "already taken" rather than
//             failing, because that is the normal race outcome.
//   commit  : the explicit completion signal carrying the manifest bytes. NOTHING else in the writer may
//             publish the manifest.
//
// RANDOM ACCESS: two writers (the sparse page dump and the socket-queue capture) emit a header, stream a
// payload whose length is only known afterwards, then patch the header in place. That is expressed as
// write_at + tell rather than seek, so a non-seekable sink can satisfy it by buffering the object.
//
// ORDERING CONTRACT the writer relies on (and any implementation must honour):
//   - an object is complete only after finish;
//   - a group's objects are invisible until group_commit;
//   - commit happens last, after every group and object of the image.

#ifndef HL_LINUX_ABI_CKPT_SINK_H
#define HL_LINUX_ABI_CKPT_SINK_H

struct ckpt_sink;

#define CKPT_SINK_BUFFER 65536u

// One open object; the writer holds an opaque pointer to it.
struct ckpt_sink_stream {
    struct ckpt_sink *sink;
    uint64_t position; // logical end-of-stream (the sequential write cursor)
    uint32_t flags;
    int failed;
    size_t buffered; // bytes held in `buffer`, logically at [position - buffered, position)
    unsigned char buffer[CKPT_SINK_BUFFER];
    uint64_t id; // channel-local object handle carried in every request
};

enum {
    // finish() must publish the object atomically. Used for image-level objects a concurrent peer or the
    // coordinator may observe while they are still being written.
    CKPT_SINK_PUBLISH_ATOMIC = 1u << 0,
};

typedef struct ckpt_sink_vtable {
    // Objects. `group` is NULL for an image-level object, else the name passed to group_begin.
    int (*begin)(struct ckpt_sink *sink, const char *group, const char *name, uint32_t flags,
                 struct ckpt_sink_stream **out);
    int (*write)(struct ckpt_sink_stream *stream, const void *data, size_t size);
    int (*write_at)(struct ckpt_sink_stream *stream, uint64_t offset, const void *data, size_t size);
    int64_t (*tell)(struct ckpt_sink_stream *stream);
    int (*finish)(struct ckpt_sink_stream *stream); // durable + visible; frees the stream either way
    void (*abort)(struct ckpt_sink_stream *stream); // discard; frees the stream

    // Groups (per-process images).
    int (*group_begin)(struct ckpt_sink *sink, const char *group);
    int (*group_commit)(struct ckpt_sink *sink, const char *group);
    void (*group_abort)(struct ckpt_sink *sink, const char *group);

    // Exclusive election marker: 0 acquired, 1 already held by someone else, -1 error.
    int (*claim)(struct ckpt_sink *sink, const char *name);
    void (*unclaim)(struct ckpt_sink *sink, const char *name);

    // Peer rendezvous. The coordinator waits for every peer's group and then counts what was published.
    // Answered by the server, which is the one place that observes every group_commit.
    int (*group_present)(struct ckpt_sink *sink, const char *group); // 1 present, 0 absent, -1 error
    int (*group_count)(struct ckpt_sink *sink, const char *prefix);  // >=0 count, -1 error

    // The image digest that authenticates the checkpoint. Called once, by the
    // coordinator, immediately before commit.
    int (*digest)(struct ckpt_sink *sink, uint64_t *hash, uint64_t *files, uint64_t *bytes);

    // Explicit completion of the whole image. Nothing may be emitted afterwards.
    int (*commit)(struct ckpt_sink *sink, const void *manifest, size_t size);
} ckpt_sink_vtable;

struct ckpt_sink {
    const ckpt_sink_vtable *ops;
};

static struct ckpt_sink g_ckpt_sink;

static struct ckpt_sink *ckpt_sink_current(void) {
    return g_ckpt_sink.ops ? &g_ckpt_sink : NULL;
}

// ---------------------------------------------------------------- generic entry points

static int ckpt_sink_begin(struct ckpt_sink *sink, const char *group, const char *name, uint32_t flags,
                           struct ckpt_sink_stream **out) {
    *out = NULL;
    if (!sink || !sink->ops) return -1;
    return sink->ops->begin(sink, group, name, flags, out);
}

static int ckpt_sink_write(struct ckpt_sink *sink, struct ckpt_sink_stream *stream, const void *data, size_t size) {
    return sink->ops->write(stream, data, size);
}

static int ckpt_sink_write_at(struct ckpt_sink *sink, struct ckpt_sink_stream *stream, uint64_t offset,
                              const void *data, size_t size) {
    return sink->ops->write_at(stream, offset, data, size);
}

static int64_t ckpt_sink_tell(struct ckpt_sink *sink, struct ckpt_sink_stream *stream) {
    return sink->ops->tell(stream);
}

// Finish and clear the caller's handle (mirrors the old ckpt_close_sync(&file) idiom).
static int ckpt_sink_finish(struct ckpt_sink *sink, struct ckpt_sink_stream **stream) {
    struct ckpt_sink_stream *s = *stream;
    if (!s) return 0;
    *stream = NULL;
    return sink->ops->finish(s);
}

static void ckpt_sink_abort(struct ckpt_sink *sink, struct ckpt_sink_stream **stream) {
    struct ckpt_sink_stream *s = *stream;
    if (!s) return;
    *stream = NULL;
    sink->ops->abort(s);
}

// begin+write+finish of a whole in-memory object.
static int ckpt_sink_put(struct ckpt_sink *sink, const char *group, const char *name, uint32_t flags, const void *data,
                         size_t size) {
    struct ckpt_sink_stream *stream = NULL;
    if (ckpt_sink_begin(sink, group, name, flags, &stream) != 0) return -1;
    if (ckpt_sink_write(sink, stream, data, size) != 0) {
        ckpt_sink_abort(sink, &stream);
        return -1;
    }
    return ckpt_sink_finish(sink, &stream);
}

static int ckpt_sink_group_begin(struct ckpt_sink *sink, const char *group) {
    return sink && sink->ops ? sink->ops->group_begin(sink, group) : -1;
}

static int ckpt_sink_group_commit(struct ckpt_sink *sink, const char *group) {
    return sink->ops->group_commit(sink, group);
}

static void ckpt_sink_group_abort(struct ckpt_sink *sink, const char *group) {
    sink->ops->group_abort(sink, group);
}

static int ckpt_sink_claim(struct ckpt_sink *sink, const char *name) {
    return sink && sink->ops ? sink->ops->claim(sink, name) : -1;
}

static void ckpt_sink_unclaim(struct ckpt_sink *sink, const char *name) {
    sink->ops->unclaim(sink, name);
}

static int ckpt_sink_group_present(struct ckpt_sink *sink, const char *group) {
    return sink && sink->ops ? sink->ops->group_present(sink, group) : -1;
}

static int ckpt_sink_group_count(struct ckpt_sink *sink, const char *prefix) {
    return sink && sink->ops ? sink->ops->group_count(sink, prefix) : -1;
}

static int ckpt_sink_digest(struct ckpt_sink *sink, uint64_t *hash, uint64_t *files, uint64_t *bytes) {
    return sink && sink->ops ? sink->ops->digest(sink, hash, files, bytes) : -1;
}

static int ckpt_sink_commit(struct ckpt_sink *sink, const void *manifest, size_t size) {
    return sink && sink->ops ? sink->ops->commit(sink, manifest, size) : -1;
}

#endif
