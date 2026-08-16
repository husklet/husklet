#ifndef HL_TRANSLATOR_IDENTITY_H
#define HL_TRANSLATOR_IDENTITY_H

#include <stddef.h>
#include <stdint.h>

#include <hl/host_services.h>

/* The host-CPU value that goes into a persistent-cache key. Cache identity is its only consumer: two hosts
 * sharing a cache directory must not accept each other's host code. It lived in the public hl/codegen.h until
 * that header and the IR lowering pipeline behind it were deleted (they had no caller in src/); the numbering
 * is preserved because it is baked into every cache artifact already on disk.
 *
 * src/host/cpu.h duplicates these values as literal macros for emitters that must name the host ISA at
 * PREPROCESSOR time; the _Static_asserts in identity.c pin the two together. */
typedef enum hl_host_isa { HL_HOST_ISA_AARCH64 = 1, HL_HOST_ISA_X86_64 = 2 } hl_host_isa;

typedef struct hl_identity_digest {
    uint8_t bytes[32];
} hl_identity_digest;

uint64_t hl_identity_name(const char *name);
uint64_t hl_identity_image(const void *bytes, size_t size);
hl_identity_digest hl_identity_image_digest(const void *bytes, size_t size);
hl_identity_digest hl_identity_engine_digest(const void *build_tag, size_t build_tag_size, uint64_t translator_abi,
                                             uint32_t guest_isa, uint32_t host_isa, uint64_t modes);
hl_identity_digest hl_identity_digest_mix(hl_identity_digest program, hl_identity_digest interpreter,
                                          hl_identity_digest engine, const char *name);
int hl_identity_digest_equal(const hl_identity_digest *left, const hl_identity_digest *right);
int hl_identity_digest_empty(const hl_identity_digest *digest);
uint64_t hl_identity_file(const hl_host_file_metadata *metadata);
uint64_t hl_identity_source(const hl_host_services *services, const char *path);
uint64_t hl_identity_mix(uint64_t program, uint64_t interpreter, uint64_t engine, uint64_t name);
uint64_t hl_identity_configuration(uint64_t build, uint32_t guest_isa, uint32_t host_isa, uint64_t modes);

#endif
