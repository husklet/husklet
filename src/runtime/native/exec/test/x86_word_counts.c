/* Pins the host words each guest form emits, so a frontend change that grows the
 * memory path -- or silently drops a guard or a publication -- shows up as a
 * number rather than as a benchmark drift nobody attributes. */
#include "../src/arch/x86_64/frontend.h"

#include <stdio.h>
#include <string.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "x86_word_counts:%d: %s\n", __LINE__, #value); return 1; } } while (0)

static uint32_t host_words[16384];
static hl_x86_a64_provenance provenance[64];

/* Emits one guest block and reports the words spent on its first instruction. */
static int first_instruction_words(const uint8_t *code, size_t size, uint32_t flags,
                                   uint32_t *words) {
    hl_x86_a64_request request;
    hl_x86_a64_result result;

    memset(&request, 0, sizeof request);
    request.abi = HL_X86_A64_FRONTEND_ABI;
    request.size = sizeof request;
    request.guest_pc = 0x401000;
    request.guest_bytes = code;
    request.guest_size = size;
    request.max_instructions = HL_X86_A64_MAX_INSTRUCTIONS;
    request.host_words = host_words;
    request.host_capacity = 8192u;
    request.provenance = provenance;
    request.provenance_capacity = 64;
    request.flags = flags;
    memset(&result, 0, sizeof result);
    if (hl_x86_a64_emit(&request, &result) != HL_X86_A64_OK || result.provenance_count == 0)
        return 0;
    *words = provenance[0].word_end - provenance[0].word_start;
    return 1;
}

struct form {
    const char *name;
    const uint8_t *bytes;
    size_t size;
    /* Words for the first instruction: plain, then checkpoints|live_chain. */
    uint32_t plain;
    uint32_t chained;
};

int main(void) {
    static const uint8_t call_rel[] = {0xe8, 0x00, 0x01, 0x00, 0x00, 0xc3};
    static const uint8_t call_reg[] = {0xff, 0xd0, 0xc3};
    static const uint8_t ret_only[] = {0xc3};
    static const uint8_t jmp_reg[] = {0xff, 0xe0};
    static const uint8_t push_reg[] = {0x50, 0xc3};
    static const uint8_t pop_reg[] = {0x58, 0xc3};
    static const uint8_t leave_op[] = {0xc9, 0xc3};
    static const uint8_t store8[] = {0x48, 0x89, 0x03, 0xc3};
    static const uint8_t load8[] = {0x48, 0x8b, 0x03, 0xc3};
    /* A store and a call both spend the guarded eight-byte store; a load and a
     * return both spend the cheaper read cache, and the gap between the two is
     * the write path's cost. */
    static const struct form forms[] = {
        {"call rel32", call_rel, sizeof call_rel, 141u, 160u},
        {"call reg", call_reg, sizeof call_reg, 141u, 160u},
        {"push reg", push_reg, sizeof push_reg, 133u, 152u},
        {"store8", store8, sizeof store8, 132u, 151u},
        {"ret", ret_only, sizeof ret_only, 54u, 73u},
        {"pop reg", pop_reg, sizeof pop_reg, 50u, 69u},
        {"leave", leave_op, sizeof leave_op, 49u, 68u},
        {"load8", load8, sizeof load8, 48u, 67u},
        {"jmp reg", jmp_reg, sizeof jmp_reg, 5u, 5u},
    };
    unsigned index;
    int failed = 0;

    for (index = 0; index < sizeof forms / sizeof forms[0]; ++index) {
        const struct form *item = &forms[index];
        uint32_t plain = 0;
        uint32_t chained = 0;

        CHECK(first_instruction_words(item->bytes, item->size, 0u, &plain));
        CHECK(first_instruction_words(item->bytes, item->size,
                                      HL_X86_A64_CHECKPOINTS | HL_X86_A64_LIVE_CHAIN, &chained));
        if (plain != item->plain || chained != item->chained) {
            fprintf(stderr, "x86_word_counts: %s emits %u/%u words, expected %u/%u\n", item->name,
                    plain, chained, item->plain, item->chained);
            failed = 1;
        }
    }
    if (failed) return 1;

    printf("x86_word_counts: ok\n");
    return 0;
}
