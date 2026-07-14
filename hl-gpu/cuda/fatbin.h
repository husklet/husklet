/* Clean-room nvcc fatbin container walker: extract embedded PTX text out of a fatbin so it can be
 * handed to hl's existing PTX front-end (cuModuleLoadData). Shared by the libcuda driver shim
 * (cuModuleLoadData) and the libcudart runtime shim (__cudaRegisterFatBinary), so extraction lives
 * in ONE place. No NVIDIA source is used — the container layout is the documented/community-reversed
 * public format (see docs/ideas/CUDART_PLAN.md §2).
 *
 * Two nested layers:
 *   1. __fatBinC_Wrapper_t : { int magic=0x466243b1; int version; const void* data; void* filename; }
 *      what nvcc hands __cudaRegisterFatBinary; `data` points at the container below.
 *   2. fatbin container    : a 16-byte header (magic 0xBA55ED50) then a run of typed entries;
 *      kind 1 = PTX text, kind 2 = ELF/CUBIN (SASS). We pick the first UNCOMPRESSED kind-1 entry.
 *
 * Tier 1 scope: UNCOMPRESSED PTX only. A compressed entry (flag 0x2000) or a SASS-only fatbin
 * returns NULL -> the caller surfaces a clean INVALID_IMAGE (never a crash, never a fake success).
 * The NVIDIA LZ4-variant decompressor (Tier 1.5) is deliberately out of scope.
 */
#ifndef HL_FATBIN_H
#define HL_FATBIN_H

#include <stddef.h>
#include <stdlib.h>
#include <string.h>

#define HL_FATBIN_WRAPPER_MAGIC 0x466243b1u   /* __fatBinC_Wrapper_t.magic */
#define HL_FATBIN_MAGIC         0xba55ed50u   /* fatbin container magic     */
#define HL_FATBIN_KIND_PTX      1
#define HL_FATBIN_KIND_ELF      2
#define HL_FATBIN_FLAG_COMPRESS 0x0000000000002000ull

/* __fatBinC_Wrapper_t — what __cudaRegisterFatBinary receives. */
typedef struct {
    int         magic;      /* HL_FATBIN_WRAPPER_MAGIC */
    int         version;
    const void* data;       /* -> fatbin container (HlFatBinHeader) */
    void*       filename;
} HlFatBinWrapper;

/* fatbin container header (16 bytes). */
typedef struct __attribute__((packed)) {
    unsigned int       magic;        /* HL_FATBIN_MAGIC */
    unsigned short     version;
    unsigned short     header_size;  /* = 16 */
    unsigned long long fat_size;     /* total bytes of all entries after this header */
} HlFatBinHeader;

/* per-image entry header (64 bytes). payload (PTX text or ELF) starts at entry + header_size. */
typedef struct __attribute__((packed)) {
    unsigned short     kind;              /* 1 = PTX, 2 = ELF/CUBIN */
    unsigned short     version;
    unsigned int       header_size;       /* size of this header (payload begins after it) */
    unsigned long long payload_size;      /* bytes of payload following the header */
    unsigned int       compressed_size;   /* nonzero iff FLAG_COMPRESS set */
    unsigned int       unused0;
    unsigned short     minor;
    unsigned short     major;
    unsigned int       arch;              /* sm/compute arch, e.g. 86 */
    unsigned int       name_offset;
    unsigned int       name_size;
    unsigned long long flags;             /* bit 0x2000 = FLAG_COMPRESS */
    unsigned long long reserved0;
    unsigned long long uncompressed_size;
} HlFatBinEntry;

/* True if `image` looks like an ELF/CUBIN (SASS) blob — hl cannot execute SASS. */
static inline int hl_image_is_elf(const void* image) {
    const unsigned char* b = (const unsigned char*)image;
    return b[0] == 0x7f && b[1] == 'E' && b[2] == 'L' && b[3] == 'F';
}

/* True if `image` is a fatbin wrapper or a bare fatbin container. */
static inline int hl_image_is_fatbin(const void* image) {
    unsigned int m; memcpy(&m, image, sizeof(m));
    return m == HL_FATBIN_WRAPPER_MAGIC || m == HL_FATBIN_MAGIC;
}

/* Extract the first uncompressed PTX entry as a malloc'd NUL-terminated string (caller frees).
 * Returns NULL for: not a fatbin, no PTX entry (SASS-only), or PTX present but compressed. */
static inline char* hl_fatbin_extract_ptx(const void* image, unsigned long long* out_len) {
    if (!image) return NULL;
    unsigned int m0; memcpy(&m0, image, sizeof(m0));

    const unsigned char* container;
    if (m0 == HL_FATBIN_WRAPPER_MAGIC) {
        const HlFatBinWrapper* w = (const HlFatBinWrapper*)image;
        container = (const unsigned char*)w->data;
    } else if (m0 == HL_FATBIN_MAGIC) {
        container = (const unsigned char*)image;
    } else {
        return NULL;
    }
    if (!container) return NULL;

    HlFatBinHeader h; memcpy(&h, container, sizeof(h));
    if (h.magic != HL_FATBIN_MAGIC || h.header_size < sizeof(HlFatBinHeader)) return NULL;

    const unsigned char* base = container + h.header_size;
    const unsigned char* end  = base + h.fat_size;
    const unsigned char* cur  = base;

    while (cur + sizeof(HlFatBinEntry) <= end) {
        HlFatBinEntry e; memcpy(&e, cur, sizeof(e));
        if (e.header_size < sizeof(HlFatBinEntry)) break;
        if ((unsigned long long)(end - cur) < e.header_size) break;
        const unsigned char* payload = cur + e.header_size;
        if ((unsigned long long)(end - payload) < e.payload_size) break;

        if (e.kind == HL_FATBIN_KIND_PTX && !(e.flags & HL_FATBIN_FLAG_COMPRESS)) {
            unsigned long long n = e.payload_size;
            /* PTX payloads are NUL-padded; trim the trailing NUL run so the copy is exact text. */
            while (n > 0 && payload[n - 1] == 0) n--;
            char* out = (char*)malloc((size_t)n + 1);
            if (!out) return NULL;
            memcpy(out, payload, (size_t)n);
            out[n] = 0;
            if (out_len) *out_len = n;
            return out;
        }
        cur = payload + e.payload_size;
    }
    return NULL;
}

#endif /* HL_FATBIN_H */
