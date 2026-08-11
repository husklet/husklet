// hl/linux_abi -- checkpoint SOURCE: the read side of the sink, and the only way the restore driver is
// allowed to obtain image bytes.
//
// Restore does not consume the image as a stream: it opens objects BY NAME, seeks inside them, and
// enumerates them to discover the process tree. So the source is deliberately not the mirror of the sink's
// byte stream -- it is a small random-access, enumerable interface answered by the embedder over the same
// channel the sink writes to:
//
//   size(name)                 -> object length, or -1 when it does not exist
//   read(name, offset, out, n) -> bytes, short at end of object
//   list(prefix)               -> the object names under a prefix
//   digest()                   -> the image digest, to authenticate the manifest
//
// THE FILE* SEAM. The restore driver is ~40 call sites of fopen/fread/fseek over image objects. Rewriting
// all of them into an explicit cursor API would be a large, risky, behaviour-preserving-by-inspection
// change, so instead an object is materialised into memory and handed back as a FILE* over that memory.
// One object at a time is held in the restoring process's address space -- the honest cost of a store that
// cannot be mapped or seeked, bounded by the largest single object (a process's `pages` image).

#ifndef HL_LINUX_ABI_CKPT_SOURCE_H
#define HL_LINUX_ABI_CKPT_SOURCE_H

#include "ckpt_sink.h"

struct ckpt_source;

typedef struct ckpt_source_vtable {
    int64_t (*size)(struct ckpt_source *source, const char *name);
    int64_t (*read)(struct ckpt_source *source, const char *name, uint64_t offset, void *out, size_t size);
    // Writes NUL-terminated names into `out`; returns the count, or -1.
    int (*list)(struct ckpt_source *source, const char *prefix, char *out, size_t capacity);
    int (*digest)(struct ckpt_source *source, uint64_t *hash, uint64_t *files, uint64_t *bytes);
} ckpt_source_vtable;

struct ckpt_source {
    const ckpt_source_vtable *ops;
};

static struct ckpt_source g_ckpt_source;

static struct ckpt_source *ckpt_source_current(void) {
    return g_ckpt_source.ops ? &g_ckpt_source : NULL;
}

static int64_t ckpt_source_stream_size(struct ckpt_source *source, const char *name) {
    hl_ckpt_reply reply;
    (void)source;
    int status = ckpt_stream_call(HL_CKPT_OP_SOURCE_SIZE, name, 0, 0, 0, NULL, 0, &reply, NULL, 0);
    if (status != HL_CKPT_STATUS_OK) return -1;
    return (int64_t)reply.value;
}

static int64_t ckpt_source_stream_read(struct ckpt_source *source, const char *name, uint64_t offset, void *out,
                                       size_t size) {
    size_t done = 0;
    (void)source;
    while (done < size) {
        hl_ckpt_reply reply;
        size_t chunk = size - done;
        if (chunk > HL_CKPT_STREAM_PAYLOAD_MAX) chunk = HL_CKPT_STREAM_PAYLOAD_MAX;
        if (ckpt_stream_call(HL_CKPT_OP_SOURCE_READ, name, 0, offset + done, 0, NULL, chunk, &reply, (char *)out + done,
                             chunk) != HL_CKPT_STATUS_OK)
            return -1;
        if (reply.length == 0) break; // end of object
        done += (size_t)reply.length;
    }
    return (int64_t)done;
}

static int ckpt_source_stream_list(struct ckpt_source *source, const char *prefix, char *out, size_t capacity) {
    hl_ckpt_reply reply;
    (void)source;
    if (ckpt_stream_call(HL_CKPT_OP_SOURCE_LIST, prefix, 0, 0, 0, NULL, 0, &reply, out, capacity) != HL_CKPT_STATUS_OK)
        return -1;
    return (int)reply.value;
}

static int ckpt_source_stream_digest(struct ckpt_source *source, uint64_t *hash, uint64_t *files, uint64_t *bytes) {
    hl_ckpt_stream_digest digest = {0};
    hl_ckpt_reply reply;
    (void)source;
    if (ckpt_stream_call(HL_CKPT_OP_DIGEST, NULL, 0, 0, 0, NULL, 0, &reply, &digest, sizeof digest) !=
            HL_CKPT_STATUS_OK ||
        reply.length != sizeof digest)
        return -1;
    *hash = digest.hash;
    *files = digest.files;
    *bytes = digest.bytes;
    return 0;
}

static const ckpt_source_vtable g_ckpt_source_stream_ops = {
    .size = ckpt_source_stream_size,
    .read = ckpt_source_stream_read,
    .list = ckpt_source_stream_list,
    .digest = ckpt_source_stream_digest,
};

// ---------------------------------------------------------------- binding and the FILE* seam

// Fails (NULL) when no broker descriptor was inherited from activation: there is nowhere to read from.
static struct ckpt_source *ckpt_source_bind(void) {
    if (hl_ckpt_channel_broker() < 0) return NULL;
    g_ckpt_source.ops = &g_ckpt_source_stream_ops;
    return &g_ckpt_source;
}

static int64_t ckpt_source_object_size(const char *name) {
    struct ckpt_source *source = ckpt_source_current();
    return source ? source->ops->size(source, name) : -1;
}

// Whole-object load. `size` bytes exactly, or -1.
static int ckpt_source_load(const char *name, void *out, size_t size) {
    struct ckpt_source *source = ckpt_source_current();
    int64_t actual;
    if (!source) return -1;
    actual = source->ops->size(source, name);
    if (actual < 0 || (uint64_t)actual < size) return -1;
    return source->ops->read(source, name, 0, out, size) == (int64_t)size ? 0 : -1;
}

static int ckpt_source_list(const char *prefix, char *out, size_t capacity) {
    struct ckpt_source *source = ckpt_source_current();
    return source ? source->ops->list(source, prefix, out, capacity) : -1;
}

static int ckpt_source_digest(uint64_t *hash, uint64_t *files, uint64_t *bytes) {
    struct ckpt_source *source = ckpt_source_current();
    return source ? source->ops->digest(source, hash, files, bytes) : -1;
}

// The materialised objects handed out as FILE*. One entry per open image object; the restore driver never
// holds more than a handful at once.
#define CKPT_SOURCE_OPEN_MAX 64

static struct {
    FILE *file;
    void *bytes;
} g_ckpt_source_open[CKPT_SOURCE_OPEN_MAX];

// Open an image object for reading. Mirrors fopen(name, "rb") over the materialised bytes.
static FILE *ckpt_source_fopen(const char *name) {
    struct ckpt_source *source = ckpt_source_current();
    int64_t size;
    if (!source) return NULL;
    size = source->ops->size(source, name);
    if (size < 0) return NULL;
    void *bytes = malloc((size_t)size == 0 ? 1 : (size_t)size);
    if (!bytes) return NULL;
    if (size != 0 && source->ops->read(source, name, 0, bytes, (size_t)size) != size) {
        free(bytes);
        return NULL;
    }
    FILE *file = fmemopen(bytes, (size_t)size, "rb");
    if (!file) {
        free(bytes);
        return NULL;
    }
    for (int index = 0; index < CKPT_SOURCE_OPEN_MAX; ++index)
        if (g_ckpt_source_open[index].file == NULL) {
            g_ckpt_source_open[index].file = file;
            g_ckpt_source_open[index].bytes = bytes;
            return file;
        }
    fclose(file);
    free(bytes);
    return NULL;
}

// Close a handle from ckpt_source_fopen, releasing the materialised bytes if there were any.
static int ckpt_source_fclose(FILE *file) {
    if (!file) return 0;
    for (int index = 0; index < CKPT_SOURCE_OPEN_MAX; ++index)
        if (g_ckpt_source_open[index].file == file) {
            void *bytes = g_ckpt_source_open[index].bytes;
            g_ckpt_source_open[index].file = NULL;
            g_ckpt_source_open[index].bytes = NULL;
            int result = fclose(file);
            free(bytes);
            return result;
        }
    return fclose(file);
}

#endif
