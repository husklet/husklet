#ifndef HL_TRANSLATOR_IDENTITY_H
#define HL_TRANSLATOR_IDENTITY_H

#include <stdint.h>

#include <hl/host_services.h>

/* The host-CPU value that goes into a persistent-cache key. Cache identity is its only consumer: two hosts
 * sharing a cache directory must not accept each other's host code. It lived in the public hl/codegen.h until
 * that header and the IR lowering pipeline behind it were deleted (they had no caller in src/); the numbering
 * is preserved because it is baked into every cache artifact already on disk.
 *
 * src/host/host_cpu.h duplicates these values as literal macros for emitters that must name the host ISA at
 * PREPROCESSOR time; the _Static_asserts in identity.c pin the two together. */
typedef enum hl_host_isa { HL_HOST_ISA_AARCH64 = 1, HL_HOST_ISA_X86_64 = 2 } hl_host_isa;

uint64_t hl_identity_name(const char *name);
uint64_t hl_identity_file(const hl_host_file_metadata *metadata);
uint64_t hl_identity_source(const hl_host_services *services, const char *path);
uint64_t hl_identity_mix(uint64_t program, uint64_t interpreter, uint64_t engine, uint64_t name);
uint64_t hl_identity_configuration(uint64_t build, uint32_t guest_isa, uint32_t host_isa, uint64_t modes);

#endif
