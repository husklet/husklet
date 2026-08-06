#ifndef HL_NATIVE_X86_64_FRONTEND_H
#define HL_NATIVE_X86_64_FRONTEND_H

#include <stddef.h>
#include <stdint.h>

#define HL_X86_A64_FRONTEND_ABI 1u
#define HL_X86_A64_MAX_INSTRUCTIONS 64u
#define HL_X86_A64_CHECKPOINTS 1u
#define HL_X86_A64_CONDITIONAL_SELF_LOOP 2u
/* Retains guest registers in x0..x15 and remaining budget in x26 across
 * internal chain entries. Requires HL_X86_A64_CHECKPOINTS. */
#define HL_X86_A64_LIVE_CHAIN 4u
/* Host guarantees FEAT_LSE single-copy atomic instructions. */
#define HL_X86_A64_LSE 8u
/* Emits diagnostics-only vector-dirty observation stores. */
#define HL_X86_A64_DIAGNOSTICS 16u
/* Host guarantees FEAT_AES round instructions. */
#define HL_X86_A64_AES 32u

typedef enum hl_x86_a64_status {
    HL_X86_A64_OK = 0,
    HL_X86_A64_ARGUMENT = 1,
    HL_X86_A64_TRUNCATED = 2,
    HL_X86_A64_CAPACITY = 3,
    HL_X86_A64_UNSUPPORTED = 4,
    HL_X86_A64_FLAGS_ABI_REQUIRED = 5,
} hl_x86_a64_status;

typedef enum hl_x86_a64_exit {
    HL_X86_A64_FALLTHROUGH = 0,
    HL_X86_A64_DIRECT_BRANCH = 1,
    HL_X86_A64_SYSCALL = 2,
    HL_X86_A64_INTERPRETER = 3,
    HL_X86_A64_CONDITIONAL_BRANCH = 4,
    HL_X86_A64_DYNAMIC_BRANCH = 5,
    HL_X86_A64_DIRECT_CALL = 6,
} hl_x86_a64_exit;

typedef struct hl_x86_a64_provenance {
    uint64_t guest_pc;
    uint32_t guest_size;
    uint32_t word_start;
    uint32_t word_end;
    uint32_t reserved;
} hl_x86_a64_provenance;

typedef struct hl_x86_a64_request {
    uint32_t abi;
    uint32_t size;
    uint64_t guest_pc;
    const uint8_t *guest_bytes;
    size_t guest_size;
    uint32_t max_instructions;
    uint32_t *host_words;
    size_t host_capacity;
    hl_x86_a64_provenance *provenance;
    size_t provenance_capacity;
    uint32_t flags;
    uint32_t reserved;
} hl_x86_a64_request;

typedef struct hl_x86_a64_result {
    uint32_t abi;
    uint32_t size;
    uint64_t source_start;
    uint64_t source_end;
    uint64_t exit_pc;
    uint64_t branch_target;
    uint32_t instruction_count;
    uint32_t word_count;
    uint32_t provenance_count;
    hl_x86_a64_exit exit;
} hl_x86_a64_result;

/* Emits one bounded body fragment using the retained x0..x15 guest-register
 * convention. The shared dispatcher owns entry/return stubs and cache
 * publication; this function performs no callbacks and retains no pointers. */
hl_x86_a64_status hl_x86_a64_emit(const hl_x86_a64_request *, hl_x86_a64_result *);

#endif
