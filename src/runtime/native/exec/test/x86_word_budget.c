/* Dense memory-write blocks must be split at an instruction boundary rather than abandoned to
 * HL_X86_A64_CAPACITY, which on x86 suppresses the native run entry permanently. */
#include "../src/arch/x86_64/frontend.h"

#include <stdio.h>
#include <string.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "x86_word_budget:%d: %s\n", __LINE__, #value); return 1; } } while (0)

#define CAPACITY (8192u - 7u - 32u)

static uint32_t host_words[16384];
static hl_x86_a64_provenance provenance[128];

/* Emits `count` copies of `op` followed by ret, at guest pc 0x1000. */
static hl_x86_a64_status emit_dense(const uint8_t *op, size_t op_size, unsigned count,
                                    uint32_t flags, hl_x86_a64_result *result) {
    static uint8_t code[1024];
    size_t size = 0;
    unsigned index;
    hl_x86_a64_request request;

    for (index = 0; index < count; ++index) {
        memcpy(code + size, op, op_size);
        size += op_size;
    }
    code[size++] = 0xc3;
    memset(&request, 0, sizeof request);
    request.abi = HL_X86_A64_FRONTEND_ABI;
    request.size = sizeof request;
    request.guest_pc = 0x1000;
    request.guest_bytes = code;
    request.guest_size = size;
    request.max_instructions = HL_X86_A64_MAX_INSTRUCTIONS;
    request.host_words = host_words;
    request.host_capacity = CAPACITY;
    request.provenance = provenance;
    request.provenance_capacity = 128;
    request.flags = flags;
    memset(result, 0, sizeof *result);
    return hl_x86_a64_emit(&request, result);
}

/* A split must land on a real instruction boundary: provenance stays contiguous from the block
 * start, and the reported exit pc is the guest pc of the first instruction that was dropped. */
static int split_is_well_formed(const hl_x86_a64_result *result, size_t op_size, unsigned count) {
    uint32_t index;
    if (result->provenance_count != result->instruction_count) return 0;
    for (index = 0; index < result->provenance_count; ++index) {
        if (provenance[index].guest_pc != 0x1000u + (uint64_t)index * op_size) return 0;
        /* Only a retained trailing ret may differ in size, and only as the final entry. */
        if (provenance[index].guest_size != op_size && (index != count || index + 1u != result->provenance_count))
            return 0;
        if (provenance[index].word_end < provenance[index].word_start) return 0;
        if (index != 0 && provenance[index].word_start < provenance[index - 1u].word_end) return 0;
    }
    return 1;
}

struct dense_form {
    const char *name;
    const uint8_t *bytes;
    size_t size;
    unsigned count;
};

int main(void) {
    /* lock add %rax,(%rbx); lock xadd %rax,(%rbx); lock cmpxchg %rcx,(%rbx); mov %rax,(%rbx) */
    static const uint8_t lock_add[] = {0xf0, 0x48, 0x01, 0x03};
    static const uint8_t lock_xadd[] = {0xf0, 0x48, 0x0f, 0xc1, 0x03};
    static const uint8_t lock_cmpxchg[] = {0xf0, 0x48, 0x0f, 0xb1, 0x0b};
    static const uint8_t store[] = {0x48, 0x89, 0x03};
    static const uint8_t vector_store[] = {0xf3, 0x0f, 0x7f, 0x03};
    static const struct dense_form forms[] = {
        {"lock add", lock_add, sizeof lock_add, 40},
        {"lock xadd", lock_xadd, sizeof lock_xadd, 40},
        {"lock cmpxchg", lock_cmpxchg, sizeof lock_cmpxchg, 40},
        {"store", store, sizeof store, 60},
        {"vector store", vector_store, sizeof vector_store, 63},
    };
    static const uint32_t configurations[] = {
        HL_X86_A64_CHECKPOINTS | HL_X86_A64_LSE,
        HL_X86_A64_CHECKPOINTS | HL_X86_A64_LSE | HL_X86_A64_LIVE_CHAIN,
    };
    unsigned form;
    unsigned configuration;

    /* The reported reproducer: 36 locked ops under a live chain used to exit CAPACITY. */
    {
        hl_x86_a64_result result;
        CHECK(emit_dense(lock_add, sizeof lock_add, 36,
                         HL_X86_A64_CHECKPOINTS | HL_X86_A64_LSE | HL_X86_A64_LIVE_CHAIN,
                         &result) == HL_X86_A64_OK);
        CHECK(result.instruction_count != 0);
        CHECK(result.word_count <= CAPACITY);
    }

    for (form = 0; form < sizeof forms / sizeof forms[0]; ++form) {
        for (configuration = 0; configuration < 2u; ++configuration) {
            hl_x86_a64_result result;
            hl_x86_a64_status status = emit_dense(forms[form].bytes, forms[form].size,
                                                  forms[form].count, configurations[configuration],
                                                  &result);
            if (status != HL_X86_A64_OK) {
                fprintf(stderr, "x86_word_budget: %s config %u status %d\n", forms[form].name,
                        configuration, (int)status);
                return 1;
            }
            /* Progress must be real, and the emitted body must respect the budget it was given. */
            CHECK(result.instruction_count != 0);
            CHECK(result.word_count <= CAPACITY);
            CHECK(split_is_well_formed(&result, forms[form].size, forms[form].count));
            /* A block that was split stops short and falls through to the dropped instruction. */
            if (result.instruction_count < forms[form].count) {
                CHECK(result.exit == HL_X86_A64_FALLTHROUGH);
                CHECK(result.exit_pc ==
                      0x1000u + (uint64_t)result.instruction_count * forms[form].size);
            }
        }
    }

    /* A split block must make forward progress large enough that re-entry terminates: translating
     * one instruction per entry would still be correct but would defeat the native path. */
    {
        hl_x86_a64_result result;
        CHECK(emit_dense(lock_add, sizeof lock_add, 64,
                         HL_X86_A64_CHECKPOINTS | HL_X86_A64_LSE | HL_X86_A64_LIVE_CHAIN,
                         &result) == HL_X86_A64_OK);
        CHECK(result.instruction_count >= 32u);
    }

    printf("x86_word_budget: ok\n");
    return 0;
}
