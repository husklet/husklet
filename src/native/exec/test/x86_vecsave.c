#define _POSIX_C_SOURCE 200809L
#include "support.h"
#include "../src/arch/x86_64/frontend.h"

#include <stdio.h>
#include <sys/mman.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "x86_vecsave:%d: %s\n", __LINE__, #x); return __LINE__; } } while (0)

#if defined(__aarch64__)
static hl_native_status write_begin(void *opaque) {
    test_memory *memory = opaque;
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_WRITE) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static hl_native_status write_end(void *opaque) {
    test_memory *memory = opaque;
    __builtin___clear_cache((char *)memory->writable, (char *)memory->writable + memory->capacity);
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_EXEC) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static int contract(void) {
    static const uint8_t clean[] = {0x0f, 0x05};
    static const uint8_t dirty[] = {0x66, 0x0f, 0xef, 0xc0, 0xeb, 0x02};
    hl_native_source_span spans[] = {{0x7000, clean, sizeof clean, 7, 9},
                                     {0x7100, dirty, sizeof dirty, 7, 9},
                                     {0x7108, clean, sizeof clean, 7, 9}};
    hl_native_source source = {spans, 3, 7, 9};
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    memory.write_begin = write_begin; memory.write_end = write_end;
    hl_native_config config = test_config(&memory, 0); /* diagnostics deliberately off */
    hl_native_executor *executor = NULL;
    hl_native_x86_64_cpu state = {0};
    hl_native_cpu cpu = {.abi=HL_NATIVE_ABI,.size=sizeof cpu,.architecture=HL_NATIVE_X86_64,.state.x86_64=&state};
    hl_native_run_request request = {.abi=HL_NATIVE_ABI,.size=sizeof request,.architecture=HL_NATIVE_X86_64,
                                     .mapping_epoch=7,.budget=8,.source=&source};
    hl_native_exit output = {.abi=HL_NATIVE_ABI,.size=sizeof output};
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    for (unsigned lane=0; lane<32; ++lane) state.vectors[lane]=UINT64_C(0x1100000000000000)+lane;
    state.program=0x7000;
    CHECK(hl_native_run(executor,&cpu,&request,&output)==HL_NATIVE_OK && output.kind==HL_NATIVE_EXIT_SYSCALL);
    for (unsigned lane=0; lane<32; ++lane) CHECK(state.vectors[lane]==UINT64_C(0x1100000000000000)+lane);
    CHECK(state.vector_dirty==0);
    state.program=0x7100; state.vectors[0]=state.vectors[1]=UINT64_MAX;
    CHECK(hl_native_run(executor,&cpu,&request,&output)==HL_NATIVE_OK && output.kind==HL_NATIVE_EXIT_SYSCALL);
    CHECK(state.program==0x710a && state.vectors[0]==0 && state.vectors[1]==0 && state.vector_dirty==0);
    hl_native_destroy(executor);
    return 0;
}

static int near_capacity(void) {
    uint8_t guest[63u * 3u + 2u];
    struct { uint32_t words[8192]; uint64_t canary; } output = {{0}, UINT64_C(0xfeedfacecafebeef)};
    hl_x86_a64_provenance provenance[64] = {0};
    hl_x86_a64_result result = {.abi=HL_X86_A64_FRONTEND_ABI,.size=sizeof result};
    for (unsigned index=0; index<63; ++index) {
        guest[index*3]=0x0f; guest[index*3+1]=0xb1; guest[index*3+2]=0x00; /* cmpxchg [rax],eax */
    }
    guest[189]=0x0f; guest[190]=0x05;
    hl_x86_a64_request request = {.abi=HL_X86_A64_FRONTEND_ABI,.size=sizeof request,.guest_pc=0x8000,
        .guest_bytes=guest,.guest_size=sizeof guest,.max_instructions=64,.host_words=output.words,
        .host_capacity=8192-7-64,.provenance=provenance,.provenance_capacity=64,
        .flags=HL_X86_A64_CHECKPOINTS|HL_X86_A64_LIVE_CHAIN|HL_X86_A64_LSE};
    hl_x86_a64_status status=hl_x86_a64_emit(&request,&result);
    CHECK(status==HL_X86_A64_OK || status==HL_X86_A64_CAPACITY);
    if (status==HL_X86_A64_OK) {
        size_t exact=result.word_count;
        CHECK(exact<=request.host_capacity);
        request.host_capacity=exact;
        CHECK(hl_x86_a64_emit(&request,&result)==HL_X86_A64_OK && result.word_count==exact);
        CHECK(output.canary==UINT64_C(0xfeedfacecafebeef));
        request.host_capacity=exact-1;
        CHECK(hl_x86_a64_emit(&request,&result)==HL_X86_A64_CAPACITY);
    }
    CHECK(output.canary==UINT64_C(0xfeedfacecafebeef));
    return 0;
}
#endif

int main(void) {
#if defined(__aarch64__)
    { int status=near_capacity(); return status != 0 ? status : contract(); }
#else
    return 0;
#endif
}
