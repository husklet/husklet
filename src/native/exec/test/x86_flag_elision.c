#include "../src/arch/x86_64/frontend.h"
#include "../src/arch/x86_64/decode.h"
#include "../src/arch/x86_64/flags.h"
#include "../include/cpu.h"
#include "../include/executor.h"

#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(expression)                                                                                              \
    do {                                                                                                               \
        if (!(expression)) {                                                                                           \
            fprintf(stderr, "x86_flag_elision:%d: %s\n", __LINE__, #expression);                                        \
            return __LINE__;                                                                                           \
        }                                                                                                              \
    } while (0)

#if defined(__aarch64__)
extern void hl_x86_test_enter(hl_native_x86_64_cpu *, void *);
#endif

static hl_x86_a64_request request_for(const uint8_t *guest, size_t size, uint32_t *host,
                                      hl_x86_a64_provenance *provenance) {
    hl_x86_a64_request request;

    memset(&request, 0, sizeof request);
    request.abi = HL_X86_A64_FRONTEND_ABI;
    request.size = sizeof request;
    request.guest_pc = UINT64_C(0x400000);
    request.guest_bytes = guest;
    request.guest_size = size;
    request.max_instructions = HL_X86_A64_MAX_INSTRUCTIONS;
    request.host_words = host;
    request.host_capacity = 4096;
    request.provenance = provenance;
    request.provenance_capacity = 64;
    request.flags = HL_X86_A64_LSE;
    return request;
}

/* Host words emitted for guest instruction `index`, or 0 when the block did not translate. */
static uint32_t words_for(const uint8_t *guest, size_t size, uint32_t index) {
    uint32_t host[4096] = {0};
    hl_x86_a64_provenance provenance[64] = {0};
    hl_x86_a64_request request = request_for(guest, size, host, provenance);
    hl_x86_a64_result result;

    if (hl_x86_a64_emit(&request, &result) != HL_X86_A64_OK) return 0;
    if (index >= result.instruction_count) return 0;
    return provenance[index].word_end - provenance[index].word_start;
}

/* Emit into exactly `capacity` host words, reporting the status the budgeting pass produced. */
static hl_x86_a64_status status_at_capacity(const uint8_t *guest, size_t size, uint32_t capacity) {
    uint32_t host[4096] = {0};
    hl_x86_a64_provenance provenance[64] = {0};
    hl_x86_a64_request request = request_for(guest, size, host, provenance);
    hl_x86_a64_result result;

    request.host_capacity = capacity;
    return hl_x86_a64_emit(&request, &result);
}

/* The budgeting pass must never promise fewer words than the emit pass writes. */
static int budget_covers_emitted(const uint8_t *guest, size_t size) {
    uint32_t host[4096] = {0};
    hl_x86_a64_provenance provenance[64] = {0};
    hl_x86_a64_request request = request_for(guest, size, host, provenance);
    hl_x86_a64_result result;

    if (hl_x86_a64_emit(&request, &result) != HL_X86_A64_OK) return 0;
    return status_at_capacity(guest, size, result.word_count - 1u) == HL_X86_A64_CAPACITY;
}

/* Stronger: the two passes agree to the word, so the elided form wastes no reserved space. */
static int budget_is_exact(const uint8_t *guest, size_t size) {
    uint32_t host[4096] = {0};
    hl_x86_a64_provenance provenance[64] = {0};
    hl_x86_a64_request request = request_for(guest, size, host, provenance);
    hl_x86_a64_result result;

    if (!budget_covers_emitted(guest, size)) return 0;
    if (hl_x86_a64_emit(&request, &result) != HL_X86_A64_OK) return 0;
    return status_at_capacity(guest, size, result.word_count) == HL_X86_A64_OK;
}

/* A register ALU op costs this many words once its flag materialization is elided. */
#define ELIDED_ADD 4u
#define ELIDED_LOGICAL 5u

static int elision_fires(void) {
    const uint8_t chain[] = {0x01, 0xc8, 0x01, 0xc8, 0x01, 0xd0}; /* add eax,ecx x2 ; add eax,edx */
    const uint8_t logical[] = {0x31, 0xc0, 0x01, 0xc8};           /* xor eax,eax ; add eax,ecx */
    const uint8_t cmp_then_sub[] = {0x39, 0xc8, 0x29, 0xd0};      /* cmp eax,ecx ; sub eax,edx */

    CHECK(words_for(chain, sizeof chain, 0) == ELIDED_ADD);
    CHECK(words_for(chain, sizeof chain, 1) == ELIDED_ADD);
    CHECK(words_for(logical, sizeof logical, 0) == ELIDED_LOGICAL);
    /* cmp is flags_only, so an elided producer keeps only its two operand moves and the subs. */
    CHECK(words_for(cmp_then_sub, sizeof cmp_then_sub, 0) == ELIDED_ADD - 1u);
    return 0;
}

/* PF and AF form a separate tail that dies to any following ALU op, including the adc/sbb and
   inc/dec forms that read or preserve CF and so cannot kill the whole flag word. */
#define PFAF_ELIDED_ADD 27u
#define PFAF_ELIDED_LOGICAL 23u
#define FULL_ADD 45u

static int pfaf_elision_fires(void) {
    const uint8_t adc[] = {0x48, 0x01, 0xc8, 0x48, 0x11, 0xd0};     /* add rax,rcx ; adc rax,rdx */
    const uint8_t sbb[] = {0x48, 0x01, 0xc8, 0x48, 0x19, 0xd0};     /* add rax,rcx ; sbb rax,rdx */
    const uint8_t increment[] = {0x48, 0x01, 0xc8, 0xff, 0xc2};     /* add rax,rcx ; inc edx */
    const uint8_t decrement[] = {0x48, 0x01, 0xc8, 0xff, 0xca};     /* add rax,rcx ; dec edx */
    const uint8_t logical[] = {0x48, 0x31, 0xd0, 0x48, 0x11, 0xd0}; /* xor rax,rdx ; adc rax,rdx */
    const uint8_t compare[] = {0x48, 0x39, 0xc8, 0x48, 0x11, 0xd0}; /* cmp rax,rcx ; adc rax,rdx */

    CHECK(words_for(adc, sizeof adc, 0) == PFAF_ELIDED_ADD);
    CHECK(words_for(sbb, sizeof sbb, 0) == PFAF_ELIDED_ADD);
    CHECK(words_for(increment, sizeof increment, 0) == PFAF_ELIDED_ADD);
    CHECK(words_for(decrement, sizeof decrement, 0) == PFAF_ELIDED_ADD);
    CHECK(words_for(logical, sizeof logical, 0) == PFAF_ELIDED_LOGICAL);
    /* cmp is flags_only, so it keeps one word fewer than the writing forms. */
    CHECK(words_for(compare, sizeof compare, 0) == PFAF_ELIDED_ADD - 6u);

    CHECK(budget_is_exact(adc, sizeof adc));
    CHECK(budget_is_exact(increment, sizeof increment));
    CHECK(budget_is_exact(logical, sizeof logical));
    return 0;
}

static int pfaf_boundaries_materialize(void) {
    const uint8_t block_end[] = {0x48, 0x01, 0xc8};                 /* add rax,rcx (last in block) */
    const uint8_t jcc[] = {0x48, 0x01, 0xc8, 0x75, 0x02};           /* add ; jne -- PF leaves the block */
    const uint8_t pushf[] = {0x48, 0x01, 0xc8, 0x9c};               /* add ; pushfq */
    const uint8_t memory[] = {0x48, 0x01, 0xc8, 0x48, 0x13, 0x03};  /* add ; adc rax,[rbx] -- may fault */
    const uint8_t syscall_exit[] = {0x48, 0x01, 0xc8, 0x0f, 0x05};  /* add ; syscall */
    const uint8_t vector[] = {0x48, 0x01, 0xc8, 0xf2, 0x0f, 0x58, 0xc1}; /* add ; addsd */

    CHECK(words_for(block_end, sizeof block_end, 0) == FULL_ADD);
    CHECK(words_for(jcc, sizeof jcc, 0) == FULL_ADD);
    CHECK(words_for(pushf, sizeof pushf, 0) == FULL_ADD);
    CHECK(words_for(memory, sizeof memory, 0) == FULL_ADD);
    CHECK(words_for(syscall_exit, sizeof syscall_exit, 0) == FULL_ADD);
    CHECK(words_for(vector, sizeof vector, 0) == FULL_ADD);
    return 0;
}

/* Shifts pay the same flag tax as ALU ops and elide down to the shifted value alone. */
#define ELIDED_SHIFT 3u
#define ELIDED_VARIABLE_SHIFT 6u

static int shift_elision_fires(void) {
    const uint8_t immediate[] = {0xc1, 0xe0, 0x03, 0x01, 0xc8}; /* shl eax,3 ; add eax,ecx */
    const uint8_t once[] = {0xd1, 0xe8, 0x01, 0xc8};            /* shr eax,1 ; add eax,ecx */
    const uint8_t variable[] = {0xd3, 0xe0, 0x01, 0xc8};        /* shl eax,cl ; add eax,ecx */
    const uint8_t wide[] = {0x48, 0xd3, 0xf8, 0x01, 0xc8};      /* sar rax,cl ; add eax,ecx */
    const uint8_t chained[] = {0xd1, 0xe0, 0xd1, 0xe0, 0x01, 0xc8}; /* shl eax,1 x2 ; add eax,ecx */

    CHECK(words_for(immediate, sizeof immediate, 0) == ELIDED_SHIFT);
    CHECK(words_for(once, sizeof once, 0) == ELIDED_SHIFT);
    CHECK(words_for(variable, sizeof variable, 0) == ELIDED_VARIABLE_SHIFT);
    CHECK(words_for(wide, sizeof wide, 0) == ELIDED_VARIABLE_SHIFT);
    /* A shift does not unconditionally define every flag, so it never kills its predecessor. */
    CHECK(words_for(chained, sizeof chained, 0) > ELIDED_SHIFT);
    CHECK(words_for(chained, sizeof chained, 1) == ELIDED_SHIFT);

    CHECK(budget_is_exact(immediate, sizeof immediate));
    CHECK(budget_is_exact(once, sizeof once));
    CHECK(budget_is_exact(variable, sizeof variable));
    CHECK(budget_is_exact(wide, sizeof wide));
    /* The unelided shift ahead of it keeps one word of pre-existing budget slack. */
    CHECK(budget_covers_emitted(chained, sizeof chained));
    return 0;
}

static int shift_boundaries_materialize(void) {
    const uint8_t block_end[] = {0xc1, 0xe0, 0x03};             /* shl eax,3 (last in block) */
    const uint8_t adc[] = {0xc1, 0xe0, 0x03, 0x11, 0xd0};       /* shl ; adc -- reads CF */
    const uint8_t jcc[] = {0xc1, 0xe0, 0x03, 0x75, 0x02};       /* shl ; jne */
    const uint8_t setcc[] = {0xc1, 0xe0, 0x03, 0x0f, 0x94, 0xc2}; /* shl ; sete dl */
    const uint8_t cmov[] = {0xc1, 0xe0, 0x03, 0x0f, 0x44, 0xd0};  /* shl ; cmove edx,eax */
    const uint8_t pushf[] = {0xc1, 0xe0, 0x03, 0x9c};           /* shl ; pushfq */
    const uint8_t inc[] = {0xc1, 0xe0, 0x03, 0xff, 0xc2};       /* shl ; inc edx -- preserves CF */
    const uint8_t memory[] = {0xc1, 0xe0, 0x03, 0x03, 0x03};    /* shl ; add eax,[rbx] -- may fault */
    const uint8_t syscall_exit[] = {0xc1, 0xe0, 0x03, 0x0f, 0x05}; /* shl ; syscall */
    const uint8_t variable_end[] = {0xd3, 0xe0};                /* shl eax,cl (last in block) */

    CHECK(words_for(block_end, sizeof block_end, 0) > ELIDED_SHIFT);
    CHECK(words_for(adc, sizeof adc, 0) > ELIDED_SHIFT);
    CHECK(words_for(jcc, sizeof jcc, 0) > ELIDED_SHIFT);
    CHECK(words_for(setcc, sizeof setcc, 0) > ELIDED_SHIFT);
    CHECK(words_for(cmov, sizeof cmov, 0) > ELIDED_SHIFT);
    CHECK(words_for(pushf, sizeof pushf, 0) > ELIDED_SHIFT);
    CHECK(words_for(inc, sizeof inc, 0) > ELIDED_SHIFT);
    CHECK(words_for(memory, sizeof memory, 0) > ELIDED_SHIFT);
    CHECK(words_for(syscall_exit, sizeof syscall_exit, 0) > ELIDED_SHIFT);
    CHECK(words_for(variable_end, sizeof variable_end, 0) > ELIDED_VARIABLE_SHIFT);
    return 0;
}

/* A rol/ror of a 32- or 64-bit register collapses to a single host rotate once flags are dead. */
#define ELIDED_ROTATE 1u

static int rotate_elision_fires(void) {
    const uint8_t left[] = {0xc1, 0xc0, 0x0d, 0x01, 0xd0};       /* rol eax,13 ; add eax,edx */
    const uint8_t right[] = {0xc1, 0xc8, 0x0d, 0x01, 0xd0};      /* ror eax,13 ; add eax,edx */
    const uint8_t wide[] = {0x48, 0xc1, 0xc2, 0x0d, 0x01, 0xc8}; /* rol rdx,13 ; add eax,ecx */
    const uint8_t left_cl[] = {0xd3, 0xc0, 0x01, 0xd0};          /* rol eax,cl ; add eax,edx */
    const uint8_t right_cl[] = {0xd3, 0xc8, 0x01, 0xd0};         /* ror eax,cl ; add eax,edx */
    const uint8_t through_carry[] = {0xd1, 0xd0, 0x01, 0xd0};    /* rcl eax,1 ; add eax,edx */

    CHECK(words_for(left, sizeof left, 0) == ELIDED_ROTATE);
    CHECK(words_for(right, sizeof right, 0) == ELIDED_ROTATE);
    CHECK(words_for(wide, sizeof wide, 0) == ELIDED_ROTATE);
    /* Left by a variable count needs the count negated first. */
    CHECK(words_for(left_cl, sizeof left_cl, 0) == ELIDED_ROTATE + 1u);
    CHECK(words_for(right_cl, sizeof right_cl, 0) == ELIDED_ROTATE);
    /* rcl reads CF into the value, so it keeps the full rotate-through-carry form. */
    CHECK(words_for(through_carry, sizeof through_carry, 0) > ELIDED_ROTATE + 1u);

    CHECK(budget_is_exact(left, sizeof left));
    CHECK(budget_is_exact(wide, sizeof wide));
    CHECK(budget_is_exact(left_cl, sizeof left_cl));
    CHECK(budget_is_exact(right_cl, sizeof right_cl));
    return 0;
}

static int rotate_boundaries_materialize(void) {
    const uint8_t block_end[] = {0xc1, 0xc0, 0x0d};              /* rol eax,13 (last in block) */
    const uint8_t adc[] = {0xc1, 0xc0, 0x0d, 0x11, 0xd0};        /* rol ; adc -- reads CF */
    const uint8_t jcc[] = {0xc1, 0xc0, 0x0d, 0x75, 0x02};        /* rol ; jne */
    const uint8_t setcc[] = {0xc1, 0xc0, 0x0d, 0x0f, 0x94, 0xc2};/* rol ; sete dl */
    const uint8_t pushf[] = {0xc1, 0xc0, 0x0d, 0x9c};            /* rol ; pushfq */
    const uint8_t inc[] = {0xc1, 0xc0, 0x0d, 0xff, 0xc2};        /* rol ; inc edx */
    const uint8_t memory[] = {0xc1, 0xc0, 0x0d, 0x03, 0x03};     /* rol ; add eax,[rbx] */
    const uint8_t syscall_exit[] = {0xc1, 0xc0, 0x0d, 0x0f, 0x05}; /* rol ; syscall */

    CHECK(words_for(block_end, sizeof block_end, 0) > ELIDED_ROTATE);
    CHECK(words_for(adc, sizeof adc, 0) > ELIDED_ROTATE);
    CHECK(words_for(jcc, sizeof jcc, 0) > ELIDED_ROTATE);
    CHECK(words_for(setcc, sizeof setcc, 0) > ELIDED_ROTATE);
    CHECK(words_for(pushf, sizeof pushf, 0) > ELIDED_ROTATE);
    CHECK(words_for(inc, sizeof inc, 0) > ELIDED_ROTATE);
    CHECK(words_for(memory, sizeof memory, 0) > ELIDED_ROTATE);
    CHECK(words_for(syscall_exit, sizeof syscall_exit, 0) > ELIDED_ROTATE);
    return 0;
}

/* imul restores the whole NZCV half, so it kills that half of a predecessor's flags. It leaves PF and
   AF alone, so the PF/AF half survives it: a rotate writes neither and collapses, an ALU op does not. */
#define FULL_LOGICAL 36u
#define NZCV_ELIDED_LOGICAL 18u

static int multiply_kills_nzcv(void) {
    const uint8_t logical[] = {0x48, 0x31, 0xd0, 0x48, 0x0f, 0xaf, 0xd1};      /* xor rax,rdx ; imul rdx,rcx */
    const uint8_t immediate[] = {0x48, 0x31, 0xd0, 0x48, 0x6b, 0xd1, 0x07};    /* xor ; imul rdx,rcx,7 */
    const uint8_t rotate[] = {0x48, 0xc1, 0xc2, 0x0d, 0x48, 0x0f, 0xaf, 0xd1}; /* rol rdx,13 ; imul */
    const uint8_t shift[] = {0x48, 0xc1, 0xe2, 0x0d, 0x48, 0x0f, 0xaf, 0xd1};  /* shl rdx,13 ; imul */
    const uint8_t memory[] = {0x48, 0x31, 0xd0, 0x48, 0x0f, 0xaf, 0x13};       /* xor ; imul rdx,[rbx] */

    CHECK(words_for(logical, sizeof logical, 0) == NZCV_ELIDED_LOGICAL);
    CHECK(words_for(immediate, sizeof immediate, 0) == NZCV_ELIDED_LOGICAL);
    CHECK(words_for(rotate, sizeof rotate, 0) == ELIDED_ROTATE);
    /* A shift defines PF, which imul does not redefine, so it keeps its whole flag word. */
    CHECK(words_for(shift, sizeof shift, 0) > ELIDED_SHIFT);
    /* A memory source can fault before the multiply redefines anything. */
    CHECK(words_for(memory, sizeof memory, 0) == FULL_LOGICAL);

    /* The multiply's own budget carries pre-existing slack, so only the covering bound is asserted. */
    CHECK(status_at_capacity(rotate, sizeof rotate, ELIDED_ROTATE) == HL_X86_A64_CAPACITY);
    return 0;
}

/* Total host words for a block emitted with the given extra request flags. */
static uint32_t block_words(const uint8_t *guest, size_t size, uint64_t extra) {
    uint32_t host[4096] = {0};
    hl_x86_a64_provenance provenance[64] = {0};
    hl_x86_a64_request request = request_for(guest, size, host, provenance);
    hl_x86_a64_result result;

    request.flags |= extra;
    if (hl_x86_a64_emit(&request, &result) != HL_X86_A64_OK) return 0;
    return result.word_count;
}

/* Checkpoints exist so an exit can report how many guest instructions retired, so only instructions
   that can exit need one. A register-only vector op carries no guard and cannot leave the block --
   except the float arithmetic forms, which bail on an unordered result. */
/* With checkpoints on, the two passes must agree to the word: one fewer must be refused. */
static int checkpointed_budget_is_exact(const uint8_t *guest, size_t size) {
    uint32_t host[4096] = {0};
    hl_x86_a64_provenance provenance[64] = {0};
    hl_x86_a64_request request = request_for(guest, size, host, provenance);
    hl_x86_a64_result result;
    uint32_t exact = block_words(guest, size, HL_X86_A64_CHECKPOINTS);

    request.flags |= HL_X86_A64_CHECKPOINTS;
    request.host_capacity = exact - 1u;
    if (hl_x86_a64_emit(&request, &result) != HL_X86_A64_CAPACITY) return 0;
    request.host_capacity = exact;
    return hl_x86_a64_emit(&request, &result) == HL_X86_A64_OK;
}

static uint32_t checkpoint_cost(const uint8_t *guest, size_t size) {
    return block_words(guest, size, HL_X86_A64_CHECKPOINTS) - block_words(guest, size, 0);
}

static int register_vectors_need_no_checkpoint(void) {
    const uint8_t control[] = {0x01, 0xd8, 0x01, 0xf1, 0x01, 0xd1};
    const uint8_t pxor[] = {0x01, 0xd8, 0x66, 0x0f, 0xef, 0xc1, 0x01, 0xd1};
    const uint8_t movaps[] = {0x01, 0xd8, 0x0f, 0x28, 0xc1, 0x01, 0xd1};
    const uint8_t paddd[] = {0x01, 0xd8, 0x66, 0x0f, 0xfe, 0xc1, 0x01, 0xd1};
    const uint8_t addsd[] = {0x01, 0xd8, 0xf2, 0x0f, 0x58, 0xc1, 0x01, 0xd1};
    const uint8_t loaded[] = {0x01, 0xd8, 0x66, 0x0f, 0xef, 0x03, 0x01, 0xd1};

    /* Only the block-end checkpoint remains, exactly as for a block holding no vector op at all. */
    CHECK(checkpoint_cost(control, sizeof control) > 0u);
    CHECK(checkpoint_cost(pxor, sizeof pxor) == checkpoint_cost(control, sizeof control));
    CHECK(checkpoint_cost(movaps, sizeof movaps) == checkpoint_cost(control, sizeof control));
    CHECK(checkpoint_cost(paddd, sizeof paddd) == checkpoint_cost(control, sizeof control));
    /* addsd can bail on an unordered result, and a memory operand can fault, so both keep theirs. */
    CHECK(checkpoint_cost(addsd, sizeof addsd) > checkpoint_cost(control, sizeof control));
    CHECK(checkpoint_cost(loaded, sizeof loaded) > checkpoint_cost(control, sizeof control));

    /* The budget pass and the emit pass must still agree to the word once the checkpoint is gone. */
    CHECK(checkpointed_budget_is_exact(pxor, sizeof pxor));
    CHECK(checkpointed_budget_is_exact(paddd, sizeof paddd));
    CHECK(checkpointed_budget_is_exact(movaps, sizeof movaps));
    return 0;
}

/* Every boundary at which the guest can observe EFLAGS must keep the producer materializing. */
static int boundaries_materialize(void) {
    const uint8_t block_end[] = {0x01, 0xc8};                   /* add eax,ecx (last in block) */
    const uint8_t adc[] = {0x01, 0xc8, 0x11, 0xd0};             /* add ; adc  -- reads CF */
    const uint8_t sbb[] = {0x01, 0xc8, 0x19, 0xd0};             /* add ; sbb  -- reads CF */
    const uint8_t jcc[] = {0x01, 0xc8, 0x75, 0x02};             /* add ; jne */
    const uint8_t setcc[] = {0x01, 0xc8, 0x0f, 0x94, 0xc2};     /* add ; sete dl */
    const uint8_t cmov[] = {0x01, 0xc8, 0x0f, 0x44, 0xd0};      /* add ; cmove edx,eax */
    const uint8_t pushf[] = {0x01, 0xc8, 0x9c};                 /* add ; pushfq */
    const uint8_t lahf[] = {0x01, 0xc8, 0x9f};                  /* add ; lahf */
    const uint8_t inc[] = {0x01, 0xc8, 0xff, 0xc2};             /* add ; inc edx -- preserves CF */
    const uint8_t memory[] = {0x01, 0xc8, 0x03, 0x03};          /* add ; add eax,[rbx] -- may fault */
    const uint8_t syscall_exit[] = {0x01, 0xc8, 0x0f, 0x05};    /* add ; syscall */
    const uint8_t rep[] = {0x01, 0xc8, 0xf3, 0xa4};             /* add ; rep movsb */

    CHECK(words_for(block_end, sizeof block_end, 0) > ELIDED_ADD);
    CHECK(words_for(adc, sizeof adc, 0) > ELIDED_ADD);
    CHECK(words_for(sbb, sizeof sbb, 0) > ELIDED_ADD);
    CHECK(words_for(jcc, sizeof jcc, 0) > ELIDED_ADD);
    CHECK(words_for(setcc, sizeof setcc, 0) > ELIDED_ADD);
    CHECK(words_for(cmov, sizeof cmov, 0) > ELIDED_ADD);
    CHECK(words_for(pushf, sizeof pushf, 0) > ELIDED_ADD);
    CHECK(words_for(lahf, sizeof lahf, 0) > ELIDED_ADD);
    CHECK(words_for(inc, sizeof inc, 0) > ELIDED_ADD);
    CHECK(words_for(memory, sizeof memory, 0) > ELIDED_ADD);
    CHECK(words_for(syscall_exit, sizeof syscall_exit, 0) > ELIDED_ADD);
    CHECK(words_for(rep, sizeof rep, 0) > ELIDED_ADD);
    return 0;
}

#if defined(__aarch64__)
/* Translate, execute, and hand back the guest flags the block leaves behind. */
static int run_block(const uint8_t *guest, size_t size, const uint64_t *registers,
                     hl_native_x86_64_cpu *cpu) {
    uint32_t host[4096] = {0};
    hl_x86_a64_provenance provenance[64] = {0};
    hl_x86_a64_request request = request_for(guest, size, host, provenance);
    hl_x86_a64_result result;
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code;
    unsigned index;

    if (hl_x86_a64_emit(&request, &result) != HL_X86_A64_OK) return 0;
    code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (code == MAP_FAILED) return 0;
    memcpy(code, host, result.word_count * sizeof(uint32_t));
    ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0); /* ret */
    __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
    if (mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) != 0) return 0;
    memset(cpu, 0, sizeof *cpu);
    for (index = 0; index < 16u; ++index) cpu->registers[index] = registers[index];
    hl_x86_test_enter(cpu, code);
    munmap(code, (size_t)page);
    return 1;
}

/* Translate with a guest memory window installed, run, and hand back the CPU the block left. */
static int run_faulting(const uint8_t *guest, size_t size, uint64_t rdx, hl_native_x86_64_cpu *cpu) {
    uint32_t host[4096] = {0};
    hl_x86_a64_provenance provenance[64] = {0};
    hl_x86_a64_request request = request_for(guest, size, host, provenance);
    hl_x86_a64_result result;
    static _Alignas(8) uint8_t bytes[32];
    long page = sysconf(_SC_PAGESIZE);
    uint8_t *code;

    if (hl_x86_a64_emit(&request, &result) != HL_X86_A64_OK) return 0;
    code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (code == MAP_FAILED) return 0;
    memcpy(code, host, result.word_count * sizeof(uint32_t));
    ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0); /* ret */
    __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
    if (mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) != 0) return 0;
    memset(cpu, 0, sizeof *cpu);
    cpu->registers[0] = UINT64_C(0xffffffffffffffff);
    cpu->registers[1] = 1u;
    cpu->registers[2] = rdx;
    cpu->memory_first = UINT64_C(0x1000);
    cpu->memory_last = UINT64_C(0x1020);
    cpu->memory_delta = (uint64_t)(uintptr_t)bytes - UINT64_C(0x1000);
    cpu->memory_permissions = 7u;
    cpu->dirty_first = UINT64_MAX;
    cpu->flags = 2u;
    hl_x86_test_enter(cpu, code);
    munmap(code, (size_t)page);
    return 1;
}

/* A memory operand can fault before it redefines anything, and the fallback exit publishes the guest
   flags as they stand -- so a producer ahead of one must have materialized. This is why a memory
   operand, and mul/div, are excluded from the killer sets. */
static int faulting_successor_sees_the_producer_flags(void) {
    const uint8_t pair[] = {0x48, 0x01, 0xc8, 0x48, 0x03, 0x02}; /* add rax,rcx ; add rax,[rdx] */
    const uint8_t single[] = {0x48, 0x01, 0xc8};                 /* add rax,rcx (block end) */
    hl_native_x86_64_cpu faulted;
    hl_native_x86_64_cpu reference;

    /* rdx sits outside the window, so the second add exits before it writes any flag. */
    CHECK(run_faulting(pair, sizeof pair, UINT64_C(0x9000), &faulted));
    CHECK(faulted.reason == HL_NATIVE_EXIT_FALLBACK);
    CHECK(faulted.program == UINT64_C(0x400003));
    CHECK(run_faulting(single, sizeof single, UINT64_C(0x9000), &reference));
    CHECK(faulted.flags == reference.flags);
    return 0;
}

/* An elided producer must leave exactly the flags its successor defines, and a CF consumer that
   follows a producer must still observe the producer's carry. */
static int execution_matches(void) {
    static const uint64_t operands[16] = {0};
    uint64_t registers[16];
    hl_native_x86_64_cpu elided;
    hl_native_x86_64_cpu reference;
    const uint8_t pair[] = {0x01, 0xc8, 0x01, 0xd0};   /* add eax,ecx ; add eax,edx */
    const uint8_t single[] = {0x01, 0xd0};             /* add eax,edx */
    const uint8_t carry[] = {0x01, 0xc8, 0x11, 0xd0};  /* add eax,ecx ; adc edx,eax? -> adc eax,edx */

    (void)operands;
    /* eax=0xffffffff, ecx=1 -> first add carries out and zeroes eax; edx=5 -> second add defines all. */
    memset(registers, 0, sizeof registers);
    registers[0] = 0xffffffffu; registers[1] = 1u; registers[2] = 5u;
    CHECK(run_block(pair, sizeof pair, registers, &elided));
    /* Reference: the successor alone, started from the state the first add produces (eax=0). */
    memset(registers, 0, sizeof registers);
    registers[0] = 0u; registers[2] = 5u;
    CHECK(run_block(single, sizeof single, registers, &reference));
    CHECK((elided.flags & HL_X86_RFLAGS_NZCV_MASK) == (reference.flags & HL_X86_RFLAGS_NZCV_MASK));
    CHECK((elided.flags & (HL_X86_RFLAGS_PF | HL_X86_RFLAGS_AF)) ==
          (reference.flags & (HL_X86_RFLAGS_PF | HL_X86_RFLAGS_AF)));
    CHECK((uint32_t)elided.registers[0] == 5u);

    /* CF consumer: 0xffffffff+1 sets CF, so adc eax,edx must land on 0+5+1. */
    memset(registers, 0, sizeof registers);
    registers[0] = 0xffffffffu; registers[1] = 1u; registers[2] = 5u;
    CHECK(run_block(carry, sizeof carry, registers, &elided));
    CHECK((uint32_t)elided.registers[0] == 6u);
    return 0;
}

/* A producer whose PF/AF alone are elided must still publish its carry, and the successor must
   still leave the PF and AF it defines. */
static int pfaf_execution_matches(void) {
    uint64_t registers[16];
    hl_native_x86_64_cpu elided;
    const uint8_t pair[] = {0x48, 0x01, 0xc8, 0x48, 0x11, 0xd0}; /* add rax,rcx ; adc rax,rdx */

    /* rax=~0, rcx=1 -> the add carries out and zeroes rax, so the adc must see CF and add one.
       0+7+1 = 8: one low-byte bit set, and no carry out of bit 3. */
    memset(registers, 0, sizeof registers);
    registers[0] = UINT64_MAX; registers[1] = 1u; registers[2] = 7u;
    CHECK(run_block(pair, sizeof pair, registers, &elided));
    CHECK(elided.registers[0] == 8u);
    CHECK((elided.flags & HL_X86_RFLAGS_PF) == 0u);
    CHECK((elided.flags & HL_X86_RFLAGS_AF) == 0u);

    /* Without a carry out of the producer the pair lands on 1+1+7 = 9, whose low byte has even parity. */
    memset(registers, 0, sizeof registers);
    registers[0] = 1u; registers[1] = 1u; registers[2] = 7u;
    CHECK(run_block(pair, sizeof pair, registers, &elided));
    CHECK(elided.registers[0] == 9u);
    CHECK((elided.flags & HL_X86_RFLAGS_PF) != 0u);
    CHECK((elided.flags & HL_X86_RFLAGS_AF) == 0u);

    /* A carry out of bit 3 in the successor must still reach AF. */
    memset(registers, 0, sizeof registers);
    registers[0] = 8u; registers[1] = 0u; registers[2] = 8u;
    CHECK(run_block(pair, sizeof pair, registers, &elided));
    CHECK(elided.registers[0] == 16u);
    CHECK((elided.flags & HL_X86_RFLAGS_AF) != 0u);
    return 0;
}

/* An elided shift must still produce the shifted value, and the successor must define the flags. */
static int shift_execution_matches(void) {
    uint64_t registers[16];
    hl_native_x86_64_cpu elided;
    hl_native_x86_64_cpu reference;
    const uint8_t pair[] = {0xc1, 0xe0, 0x04, 0x01, 0xd0};      /* shl eax,4 ; add eax,edx */
    const uint8_t variable[] = {0xd3, 0xe8, 0x01, 0xd0};        /* shr eax,cl ; add eax,edx */
    const uint8_t wide[] = {0x48, 0xd3, 0xe0, 0x01, 0xd0};      /* shl rax,cl ; add eax,edx */
    const uint8_t single[] = {0x01, 0xd0};                      /* add eax,edx */

    /* eax=0x12345678 <<4 = 0x23456780; edx=5. */
    memset(registers, 0, sizeof registers);
    registers[0] = 0x12345678u; registers[2] = 5u;
    CHECK(run_block(pair, sizeof pair, registers, &elided));
    CHECK((uint32_t)elided.registers[0] == 0x23456785u);
    memset(registers, 0, sizeof registers);
    registers[0] = 0x23456780u; registers[2] = 5u;
    CHECK(run_block(single, sizeof single, registers, &reference));
    CHECK((elided.flags & HL_X86_RFLAGS_NZCV_MASK) == (reference.flags & HL_X86_RFLAGS_NZCV_MASK));
    CHECK((elided.flags & (HL_X86_RFLAGS_PF | HL_X86_RFLAGS_AF)) ==
          (reference.flags & (HL_X86_RFLAGS_PF | HL_X86_RFLAGS_AF)));

    /* Variable count, including the count-zero form that leaves the value untouched. */
    memset(registers, 0, sizeof registers);
    registers[0] = 0xff00u; registers[1] = 8u; registers[2] = 5u;
    CHECK(run_block(variable, sizeof variable, registers, &elided));
    CHECK((uint32_t)elided.registers[0] == 0xffu + 5u);
    memset(registers, 0, sizeof registers);
    registers[0] = 0xff00u; registers[1] = 0u; registers[2] = 5u;
    CHECK(run_block(variable, sizeof variable, registers, &elided));
    CHECK((uint32_t)elided.registers[0] == 0xff00u + 5u);

    /* A 64-bit variable shift keeps the full width before the 32-bit successor truncates. */
    memset(registers, 0, sizeof registers);
    registers[0] = 1u; registers[1] = 40u; registers[2] = 5u;
    CHECK(run_block(wide, sizeof wide, registers, &elided));
    CHECK(elided.registers[0] == 5u);
    return 0;
}

static uint32_t rol32(uint32_t value, unsigned count) {
    return count == 0u ? value : (uint32_t)((value << count) | (value >> (32u - count)));
}

static uint64_t rol64(uint64_t value, unsigned count) {
    return count == 0u ? value : (value << count) | (value >> (64u - count));
}

/* The single-instruction rotate must reproduce the bit-loop's value exactly. */
static int rotate_execution_matches(void) {
    uint64_t registers[16];
    hl_native_x86_64_cpu elided;
    const uint8_t left[] = {0xc1, 0xc0, 0x0d, 0x01, 0xd0};       /* rol eax,13 ; add eax,edx */
    const uint8_t right[] = {0xc1, 0xc8, 0x0d, 0x01, 0xd0};      /* ror eax,13 ; add eax,edx */
    const uint8_t wide[] = {0x48, 0xc1, 0xc2, 0x0d, 0x48, 0x01, 0xca}; /* rol rdx,13 ; add rdx,rcx */
    const uint8_t left_cl[] = {0xd3, 0xc0, 0x01, 0xd0};          /* rol eax,cl ; add eax,edx */
    const uint8_t right_cl[] = {0xd3, 0xc8, 0x01, 0xd0};         /* ror eax,cl ; add eax,edx */

    memset(registers, 0, sizeof registers);
    registers[0] = 0x12345678u; registers[2] = 5u;
    CHECK(run_block(left, sizeof left, registers, &elided));
    CHECK((uint32_t)elided.registers[0] == rol32(0x12345678u, 13u) + 5u);

    memset(registers, 0, sizeof registers);
    registers[0] = 0x12345678u; registers[2] = 5u;
    CHECK(run_block(right, sizeof right, registers, &elided));
    CHECK((uint32_t)elided.registers[0] == rol32(0x12345678u, 32u - 13u) + 5u);

    /* 64-bit, and the destination is not the accumulator. */
    memset(registers, 0, sizeof registers);
    registers[2] = UINT64_C(0x0123456789abcdef); registers[1] = 7u;
    CHECK(run_block(wide, sizeof wide, registers, &elided));
    CHECK(elided.registers[2] == rol64(UINT64_C(0x0123456789abcdef), 13u) + 7u);

    /* Variable counts, including zero, which must leave the value untouched. */
    memset(registers, 0, sizeof registers);
    registers[0] = 0x12345678u; registers[1] = 13u; registers[2] = 5u;
    CHECK(run_block(left_cl, sizeof left_cl, registers, &elided));
    CHECK((uint32_t)elided.registers[0] == rol32(0x12345678u, 13u) + 5u);

    memset(registers, 0, sizeof registers);
    registers[0] = 0x12345678u; registers[1] = 0u; registers[2] = 5u;
    CHECK(run_block(left_cl, sizeof left_cl, registers, &elided));
    CHECK((uint32_t)elided.registers[0] == 0x12345678u + 5u);

    memset(registers, 0, sizeof registers);
    registers[0] = 0x12345678u; registers[1] = 13u; registers[2] = 5u;
    CHECK(run_block(right_cl, sizeof right_cl, registers, &elided));
    CHECK((uint32_t)elided.registers[0] == rol32(0x12345678u, 32u - 13u) + 5u);

    /* A count above the width is masked by the guest, exactly as the host rotate masks it. */
    memset(registers, 0, sizeof registers);
    registers[0] = 0x12345678u; registers[1] = 45u; registers[2] = 5u;
    CHECK(run_block(left_cl, sizeof left_cl, registers, &elided));
    CHECK((uint32_t)elided.registers[0] == rol32(0x12345678u, 45u % 32u) + 5u);
    return 0;
}
#endif

int main(void) {
    int status;

    if ((status = elision_fires()) != 0) return status;
    if ((status = boundaries_materialize()) != 0) return status;
    if ((status = pfaf_elision_fires()) != 0) return status;
    if ((status = pfaf_boundaries_materialize()) != 0) return status;
    if ((status = shift_elision_fires()) != 0) return status;
    if ((status = shift_boundaries_materialize()) != 0) return status;
    if ((status = rotate_elision_fires()) != 0) return status;
    if ((status = rotate_boundaries_materialize()) != 0) return status;
    if ((status = multiply_kills_nzcv()) != 0) return status;
    if ((status = register_vectors_need_no_checkpoint()) != 0) return status;
#if defined(__aarch64__)
    if ((status = faulting_successor_sees_the_producer_flags()) != 0) return status;
    if ((status = execution_matches()) != 0) return status;
    if ((status = pfaf_execution_matches()) != 0) return status;
    if ((status = shift_execution_matches()) != 0) return status;
    if ((status = rotate_execution_matches()) != 0) return status;
#endif
    return 0;
}
