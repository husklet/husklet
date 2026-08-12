#include "identity.h"

#include "../host/host_cpu.h"

#include <stddef.h>
#include <string.h>

/* host_cpu.h duplicates the hl_host_isa numbering as literal macros because callers must name the host ISA at
 * PREPROCESSOR time, from emitters that cannot include this header. Drift is not a compile error but a
 * cache-identity hash that stops distinguishing hosts -- one host executing another's machine code. */
_Static_assert(HL_HOST_CPU_ISA_AARCH64 == HL_HOST_ISA_AARCH64, "host_cpu.h and hl_host_isa disagree on aarch64");
_Static_assert(HL_HOST_CPU_ISA_X86_64 == HL_HOST_ISA_X86_64, "host_cpu.h and hl_host_isa disagree on x86_64");

#define HL_IDENTITY_SEED 1469598103934665603ull
#define HL_IDENTITY_PRIME 1099511628211ull

static uint64_t identity_bytes(uint64_t value, const char *bytes) {
    while (*bytes != '\0') {
        value ^= (uint8_t)*bytes++;
        value *= HL_IDENTITY_PRIME;
    }
    return value;
}

uint64_t hl_identity_name(const char *name) {
    const char *base;
    const char *cursor;

    if (name == NULL) return 0x1357ull;
    base = name;
    for (cursor = name; *cursor != '\0'; ++cursor)
        if (*cursor == '/') base = cursor + 1;
    return identity_bytes(HL_IDENTITY_SEED, base);
}

uint64_t hl_identity_file(const hl_host_file_metadata *metadata) {
    uint64_t value = HL_IDENTITY_SEED;
    uint64_t fields[5];
    size_t index;
    if (metadata == NULL || metadata->type != HL_HOST_FILE_TYPE_REGULAR) return 0;
    fields[0] = metadata->stable_device;
    fields[1] = metadata->stable_object;
    fields[2] = metadata->size;
    fields[3] = metadata->modified_ns / UINT64_C(1000000000);
    fields[4] = metadata->modified_ns % UINT64_C(1000000000);
    for (index = 0; index < sizeof(fields) / sizeof(fields[0]); ++index) {
        value ^= fields[index];
        value *= HL_IDENTITY_PRIME;
    }
    return value;
}

uint64_t hl_identity_source(const hl_host_services *services, const char *path) {
    hl_host_file_metadata metadata;
    hl_host_result opened, result;
    uint64_t value;
    if (services == NULL || services->file == NULL || services->file->abi != HL_HOST_FILE_ABI ||
        services->file->size < sizeof(*services->file) || services->file->open_relative == NULL ||
        services->file->metadata == NULL || services->file->close == NULL || path == NULL || path[0] == 0)
        return 0;
    opened = services->file->open_relative(services->context, HL_HOST_HANDLE_CWD, path, strlen(path),
                                           HL_HOST_FILE_READ | HL_HOST_FILE_NOFOLLOW, 0, 0);
    if (opened.status != HL_STATUS_OK) return 0;
    result = services->file->metadata(services->context, opened.value, &metadata);
    if (services->file->close(services->context, opened.value).status != HL_STATUS_OK ||
        result.status != HL_STATUS_OK || metadata.type != HL_HOST_FILE_TYPE_REGULAR)
        return 0;
    value = hl_identity_file(&metadata);
    value ^= metadata.changed_ns;
    value *= HL_IDENTITY_PRIME;
    return identity_bytes(value, path);
}

uint64_t hl_identity_mix(uint64_t program, uint64_t interpreter, uint64_t engine, uint64_t name) {
    return (program ^ (interpreter * HL_IDENTITY_PRIME)) ^ engine ^ (name * HL_IDENTITY_PRIME);
}

uint64_t hl_identity_configuration(uint64_t build, uint32_t guest_isa, uint32_t host_isa, uint64_t modes) {
    uint64_t value = HL_IDENTITY_SEED;
    const uint64_t fields[] = {build, guest_isa, host_isa, modes};
    size_t index;
    for (index = 0; index < sizeof(fields) / sizeof(fields[0]); ++index) {
        value ^= fields[index];
        value *= HL_IDENTITY_PRIME;
    }
    return value;
}
