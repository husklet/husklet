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
#endif

int main(void) {
    int status;

    if ((status = elision_fires()) != 0) return status;
    if ((status = boundaries_materialize()) != 0) return status;
#if defined(__aarch64__)
    if ((status = execution_matches()) != 0) return status;
#endif
    return 0;
}
